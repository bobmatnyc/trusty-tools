//! Chat-thread attachment upload, listing and retrieval (#7370).
//!
//! Why: epic #7425 item (e) puts a thread's files at
//! `<assistant home>/attachments/<session>/<file>`. That directory is inside
//! the user's own home tree, next to `okg` — so it can never be exposed as a
//! static file mount, which would serve any path under it to anyone who could
//! guess a name. Every read here goes through an ID: the caller names an
//! attachment, the manifest answers with the path, and the server opens THAT.
//! A path the caller supplied is never opened.
//!
//! What: three routes under `/api/agents/{name}/sessions/{session}/attachments`
//! — `POST` (multipart upload), `GET` (the session's manifest), and
//! `GET /{id}` (the bytes). Writes go through the router-wide same-origin guard
//! and the optional bearer layer like every other mutating route
//! (`super::routes`).
//!
//! Agent identity: the agent name is validated as an
//! [`AssistantInstanceId`] and resolved to a home under
//! [`super::state::AppState::attachments_root`]. It is deliberately NOT checked
//! against the agent roster — `submit_task`'s attendance hook does not either
//! (#4703), and the store needs no second source of truth for "does this agent
//! exist". The id validation is what confines the path; the roster would only
//! add a second answer that can drift from it.
//!
//! Test: `super::tests::attachments`.

use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path as AxumPath, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use super::state::AppState;
use crate::assistants::{AssistantHome, AssistantInstanceId};
use crate::attachments::{Attachment, AttachmentError, AttachmentStore, MAX_ATTACHMENT_BYTES};

/// Largest request body the upload route accepts.
///
/// Why: axum's default is 2 MiB, which would refuse a legitimate attachment
/// long before [`MAX_ATTACHMENT_BYTES`] had a chance to speak. Derived from
/// that constant plus a megabyte of multipart framing so the two can never
/// drift; the transport bound is deliberately the LOOSER of the two, leaving
/// the product limit to the store, where the error message can name the file.
pub(super) const MAX_UPLOAD_BODY_BYTES: usize = MAX_ATTACHMENT_BYTES as usize + 1024 * 1024;

/// `POST /api/agents/{name}/sessions/{session}/attachments`.
///
/// Why: the one write path for a chat attachment. Multipart, because the
/// payload is a file the browser already has in a `File` object and base64 in
/// JSON would cost a third more bytes for nothing.
/// What: reads the first field carrying a filename, hands the bytes to
/// [`AttachmentStore::store`], and answers `201` with the stored row and the
/// URL that retrieves it. Every guard — traversal, absolute name, size cap —
/// answers a `4xx` with the store's own message and leaves the tree untouched;
/// nothing is renamed and retried.
/// Test: `super::tests::attachments::upload_stores_the_file_and_returns_its_row`,
/// `super::tests::attachments::upload_refuses_a_traversal_file_name`,
/// `super::tests::attachments::upload_refuses_an_oversize_file`.
pub(super) async fn upload_route(
    State(state): State<AppState>,
    AxumPath((name, session)): AxumPath<(String, String)>,
    mut multipart: Multipart,
) -> Response {
    let store = match resolve_store(&state, &name) {
        Ok(store) => store,
        Err(response) => return response,
    };

    let mut part: Option<(String, Option<String>, Vec<u8>)> = None;
    loop {
        match multipart.next_field().await {
            Ok(None) => break,
            Err(e) => return bad_request(&format!("could not read the upload body: {e}")),
            Ok(Some(field)) => {
                let file_name = field.file_name().map(str::to_string);
                let content_type = field.content_type().map(str::to_string);
                let bytes = match field.bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        return (
                            StatusCode::PAYLOAD_TOO_LARGE,
                            Json(json!({ "error": format!("could not read the uploaded file: {e}") })),
                        )
                            .into_response();
                    }
                };
                if let Some(file_name) = file_name {
                    part = Some((file_name, content_type, bytes.to_vec()));
                    break;
                }
            }
        }
    }

    let Some((file_name, content_type, bytes)) = part else {
        return bad_request("the upload carried no file field with a filename");
    };

    let stored = tokio::task::spawn_blocking(move || {
        store.store(&session, &file_name, content_type.as_deref(), &bytes)
    })
    .await;
    match stored {
        Err(e) => internal("attachment upload task failed", &e.to_string()),
        Ok(Err(e)) => refuse(e),
        Ok(Ok(row)) => (StatusCode::CREATED, Json(wire(&name, &row))).into_response(),
    }
}

/// `GET /api/agents/{name}/sessions/{session}/attachments`.
///
/// Why: a reload reads `[[attachment:<id>]]` markers out of persisted turns and
/// needs the name, media type and size behind each one to draw the card. One
/// manifest read answers every marker in the thread; a metadata request per
/// marker would not.
/// What: the session's rows, oldest first. A session with nothing stored is an
/// empty list and a `200`, not a `404` — an empty thread is an ordinary state.
/// Test: `super::tests::attachments::list_returns_the_session_manifest`.
pub(super) async fn list_route(
    State(state): State<AppState>,
    AxumPath((name, session)): AxumPath<(String, String)>,
) -> Response {
    let store = match resolve_store(&state, &name) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let listed = tokio::task::spawn_blocking(move || store.list(&session)).await;
    match listed {
        Err(e) => internal("attachment listing task failed", &e.to_string()),
        Ok(Err(e)) => refuse(e),
        Ok(Ok(rows)) => {
            let rows: Vec<Value> = rows.iter().map(|row| wire(&name, row)).collect();
            (StatusCode::OK, Json(json!({ "attachments": rows }))).into_response()
        }
    }
}

/// `GET /api/agents/{name}/sessions/{session}/attachments/{id}`.
///
/// Why: the card's thumbnail, and the expanded view behind it, need the bytes.
/// What: looks the id up in the manifest and serves the file the ROW names.
/// Content that a browser could execute in this origin is forced to download
/// rather than render: only images and the two plain text types are served
/// `inline`, everything else is `attachment`, and both carry `nosniff` plus a
/// `sandbox` CSP. Without that, uploading an HTML file would be stored XSS
/// against the API origin.
/// Test: `super::tests::attachments::download_returns_the_bytes_and_headers`,
/// `super::tests::attachments::download_rejects_an_unknown_id`.
pub(super) async fn download_route(
    State(state): State<AppState>,
    AxumPath((name, session, id)): AxumPath<(String, String, String)>,
) -> Response {
    let store = match resolve_store(&state, &name) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let read = tokio::task::spawn_blocking(move || store.read(&session, &id)).await;
    let (row, bytes) = match read {
        Err(e) => return internal("attachment read task failed", &e.to_string()),
        Ok(Err(e)) => return refuse(e),
        Ok(Ok(found)) => found,
    };

    let inline = row.media_type.starts_with("image/")
        || matches!(row.media_type.as_str(), "text/plain" | "text/csv");
    let disposition = if inline { "inline" } else { "attachment" };
    // The name is already a single path segment with no control characters
    // (`AttachmentStore::store`'s guard); quotes and backslashes are the only
    // remaining characters that could break out of the header's quoted string.
    let quoted: String = row
        .file_name
        .chars()
        .filter(|c| *c != '"' && *c != '\\')
        .collect();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, row.media_type.clone())
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(
            header::CONTENT_DISPOSITION,
            format!("{disposition}; filename=\"{quoted}\""),
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox",
        )
        .header(header::CACHE_CONTROL, "private, max-age=3600")
        .body(Body::from(bytes))
        .unwrap_or_else(|e| internal("could not build the attachment response", &e.to_string()))
}

/// The store for one agent, or the response explaining why there is none.
///
/// Why: the two failure shapes are genuinely different. A malformed agent name
/// is the caller's fault (`400`); a daemon that could not resolve a home
/// directory at all is the server's state (`503`), and reporting the second as
/// the first would send an operator looking at their request instead of their
/// `$HOME`.
/// Test: `super::tests::attachments::upload_refuses_a_traversal_agent_name`.
pub(super) fn resolve_store(state: &AppState, name: &str) -> Result<AttachmentStore, Response> {
    let id = AssistantInstanceId::new(name)
        .map_err(|e| bad_request(&format!("invalid agent name `{name}`: {e}")))?;
    let Some(root) = state.attachments_root.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "no assistant home directory could be resolved" })),
        )
            .into_response());
    };
    Ok(AttachmentStore::for_home(&AssistantHome::under(root, id)))
}

/// One attachment as the wire sees it.
///
/// `stored_path` is deliberately absent — see [`Attachment`]'s own note. `url`
/// is what a client fetches; it is built here so no client has to assemble the
/// route shape itself.
fn wire(agent: &str, row: &Attachment) -> Value {
    json!({
        "id": row.id,
        "session_id": row.session_id,
        "file_name": row.file_name,
        "media_type": row.media_type,
        "size": row.size,
        "sha256": row.sha256,
        "url": format!(
            "/api/agents/{}/sessions/{}/attachments/{}",
            urlencode(agent),
            urlencode(&row.session_id),
            row.id
        ),
    })
}

/// Percent-encode a path segment. Both segments are already validated to be
/// single ordinary path segments, so this only has to survive spaces and the
/// handful of punctuation characters an instance id permits.
fn urlencode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Map a store refusal to its status, without string-matching the message.
pub(super) fn refuse(error: AttachmentError) -> Response {
    let status = match &error {
        AttachmentError::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        AttachmentError::NotFound { .. } | AttachmentError::InvalidId(_) => StatusCode::NOT_FOUND,
        e if e.is_client_error() => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if !error.is_client_error() {
        tracing::warn!(%error, "attachments: request failed");
    }
    (status, Json(json!({ "error": error.to_string() }))).into_response()
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn internal(message: &str, detail: &str) -> Response {
    tracing::warn!(detail, "attachments: {message}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
        .into_response()
}
