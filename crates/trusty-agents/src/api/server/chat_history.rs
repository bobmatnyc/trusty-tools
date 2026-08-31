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
use std::time::Duration;

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
use crate::ctrl::pm_task::session_id_for;

/// Messages returned when the client names no `limit` — roughly 50 turns.
const DEFAULT_LIMIT: usize = 100;
/// Hard ceiling on one page, so a hand-crafted `limit` cannot ask the GUI to
/// render an unbounded session in one paint.
const MAX_LIMIT: usize = 500;

/// `?limit=<n>&until=<absolute exclusive end index>`.
///
/// `until` is an ABSOLUTE index into the session history, not an offset from
/// the newest end. That distinction is the whole correctness argument: the
/// history is append-only, so an existing message's index never moves, and a
/// page addressed by absolute index returns the same messages no matter how
/// many turns arrived since the client last asked. An offset-from-the-end
/// cursor does NOT have that property — k appends between two requests shift
/// the window by k and replay k messages the client already holds.
///
/// Omit `until` for the newest page. The response reports the `start` index it
/// used, which is exactly what the client passes back as the next `until`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct HistoryQuery {
    limit: Option<usize>,
    until: Option<usize>,
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
        q.until,
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
/// `chat_history_until_cursor_pages_backwards`,
/// `chat_history_empty_when_no_palace_bound`,
/// `chat_history_empty_when_session_absent`,
/// `chat_history_degrades_when_memory_unreachable`,
/// `chat_history_reports_a_malformed_session`,
/// `chat_history_unknown_agent_404`, `chat_history_rejects_traversal_name`.
pub(super) async fn chat_history_at(
    dirs: &[PathBuf],
    name: &str,
    memory_socket: Option<&Path>,
    limit: usize,
    until: Option<usize>,
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
            let mut body = page(&palace, &session_id, &session, limit, until);
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

/// A decoded session: the validated history plus the only timestamp it carries.
///
/// Why it exists rather than passing the raw `Value` on: making [`fetch_session`]
/// produce this is what forces the `history` array to be validated at decode
/// time. A raw `Value` invites a `.get("history").unwrap_or_default()` at the
/// use site, which renders a malformed payload as a healthy empty session — the
/// "confidently incomplete history" the owner ruled worse than none.
struct Session {
    history: Vec<Value>,
    updated_at: Value,
}

/// Slice `limit` messages ending at the absolute index `until`.
///
/// Why: the owner's volume requirement — a continuous session that never rolls
/// over must not hydrate in full on launch. Absolute indexing (see
/// [`HistoryQuery`]) is what makes paging backwards append-stable.
/// What: the window is `[end - limit, end)` where `end` is `until` clamped to
/// `total`, defaulting to `total` for the newest page. Returns the `start` it
/// used, which the client passes back as the next `until`. `has_more` reports
/// whether anything older remains. Pure, and panic-free: `end` is clamped to
/// `total` before `start` is derived from it by saturating subtraction, so the
/// range is always in bounds and non-inverted.
/// Test: `chat_history_returns_bounded_newest_slice`,
/// `chat_history_until_cursor_pages_backwards`,
/// `chat_history_clamps_an_out_of_range_until`.
fn page(
    palace: &str,
    session_id: &str,
    session: &Session,
    limit: usize,
    until: Option<usize>,
) -> Value {
    let total = session.history.len();
    let end = until.unwrap_or(total).min(total);
    let start = end.saturating_sub(limit);
    json!({
        "available": true,
        "palace": palace,
        "session_id": session_id,
        "messages": session.history[start..end].to_vec(),
        "start": start,
        "total": total,
        "has_more": start > 0,
        "updated_at": session.updated_at.clone(),
    })
}

/// Bound on the whole-session socket read.
///
/// Why not [`PROBE_TIMEOUT`]: that constant is a 2s LIVENESS budget, sized so a
/// hung daemon degrades a status pane fast. This call is a bulk read — the
/// upstream `chat_session_recall` takes no `limit`, so the daemon ships the
/// entire session, and a live `persona-izzie` already holds 2000+ messages.
/// Sizing a growing bulk transfer with a liveness budget makes rehydration fail
/// permanently once the session outgrows 2s. Bounded generously, because the
/// failure mode this guards is a hung socket, not a slow one.
const SESSION_READ_TIMEOUT: Duration = Duration::from_secs(20);

/// The wording `handle_chat_session_recall` uses for a session that does not
/// exist. Matched exactly rather than on a bare "not found", which a MISSING
/// PALACE also produces — reporting an absent palace as an absent session would
/// send an operator looking in the wrong place.
const SESSION_ABSENT_MARKER: &str = "session not found";

/// Fetch and decode the session, collapsing every failure into a client reason.
///
/// Why: `chat_session_recall` is absent from trusty-memory's direct-dispatch
/// `TOOL_METHODS` allow-list, so it is only reachable wrapped in `tools/call` —
/// the same envelope `persona_memory::call_memory_tool` writes through. That arm
/// answers `result.content[0].text` as a JSON STRING, so the payload needs
/// decoding back out, and it reports EVERY tool failure as `-32603`, so the
/// absent-session case is identified by the daemon's own message rather than by
/// code (see [`SESSION_ABSENT_MARKER`]).
/// What: `Ok(Session)` when the daemon answers with a decodable payload whose
/// `history` is an array. `Err(reason)` for an absent session, any other
/// refusal, a transport failure, an undecodable body, or a payload missing that
/// array — the last of which names the field, because a malformed answer and an
/// empty conversation must never render the same way.
/// Test: `chat_history_empty_when_session_absent`,
/// `chat_history_degrades_when_memory_unreachable`,
/// `chat_history_reports_a_malformed_session`,
/// `chat_history_absent_palace_is_not_reported_as_absent_session`.
async fn fetch_session(socket: &Path, palace: &str, session_id: &str) -> Result<Session, String> {
    let answer = trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        "tools/call",
        json!({
            "name": "chat_session_recall",
            "arguments": { "palace": palace, "session_id": session_id },
        }),
        SESSION_READ_TIMEOUT,
    )
    .await
    .map_err(|e| {
        let absent = e
            .downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>()
            .is_some_and(|rpc| rpc.message.contains(SESSION_ABSENT_MARKER));
        if absent {
            format!("no persisted chat session `{session_id}` yet")
        } else {
            describe_failure(&e, palace)
        }
    })?;

    let text = answer
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "trusty-memory answered chat_session_recall without `content[0].text`".to_string()
        })?;
    let decoded: Value = serde_json::from_str(text)
        .map_err(|e| format!("trusty-memory returned an undecodable chat session: {e}"))?;

    let history = decoded
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            format!("chat session `{session_id}` has no `history` array to rehydrate from")
        })?;
    tracing::debug!(
        palace,
        session_id,
        messages = history.len(),
        "chat_history: decoded persona session"
    );
    Ok(Session {
        history,
        updated_at: decoded.get("updated_at").cloned().unwrap_or(Value::Null),
    })
}
