//! Provider probe + chat-session CRUD handlers.
//!
//! Why: provider availability and chat-session create/list/get/delete are a
//! small, self-contained group split out of the former monolithic `chat.rs`
//! (issue #607).
//! What: `list_providers`, `CreateSessionBody`, and the session CRUD handlers,
//! moved verbatim.
//! Test: `providers_endpoint_returns_payload` and the session-CRUD HTTP tests.

use crate::web::{load_user_config, ApiError};
use crate::AppState;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Providers + sessions
// ---------------------------------------------------------------------------

/// GET /api/v1/chat/providers — report provider availability + active choice.
///
/// Why: The UI's chat panel surfaces whether the user has a local model
/// running or is hitting OpenRouter. Probing both upstreams here keeps that
/// logic on the server so the SPA stays dumb.
/// What: Calls `auto_detect_local_provider` (1s timeout) for Ollama and checks
/// for a non-empty OpenRouter key. Returns shape `{providers:[...], active}`.
/// Test: `providers_endpoint_returns_payload`.
pub(crate) async fn list_providers(State(state): State<AppState>) -> Json<Value> {
    let cfg = load_user_config().unwrap_or_default();
    let ollama_available = if cfg.local_model.enabled {
        trusty_common::auto_detect_local_provider(&cfg.local_model.base_url)
            .await
            .is_some()
    } else {
        false
    };
    let openrouter_available = !cfg.openrouter_api_key.is_empty();
    let active = state.chat_provider().await.map(|p| p.name().to_string());
    Json(json!({
        "providers": [
            {
                "name": "ollama",
                "model": cfg.local_model.model,
                "available": ollama_available,
            },
            {
                "name": "openrouter",
                "model": cfg.openrouter_model,
                "available": openrouter_available,
            }
        ],
        "active": active,
    }))
}

#[derive(Deserialize, Default)]
pub(crate) struct CreateSessionBody {
    #[serde(default)]
    title: Option<String>,
}

pub(crate) async fn create_chat_session(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<CreateSessionBody>>,
) -> Result<Json<Value>, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    let title = body.and_then(|b| b.0.title);
    let sid = store
        .create_session(title)
        .map_err(|e| ApiError::internal(format!("create session: {e:#}")))?;
    Ok(Json(json!({ "id": sid })))
}

pub(crate) async fn list_chat_sessions(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    let metas = store
        .list_sessions()
        .map_err(|e| ApiError::internal(format!("list sessions: {e:#}")))?;
    Ok(Json(serde_json::to_value(metas).unwrap_or(json!([]))))
}

pub(crate) async fn get_chat_session(
    State(state): State<AppState>,
    AxumPath((id, session_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    let s = store
        .get_session(&session_id)
        .map_err(|e| ApiError::internal(format!("get session: {e:#}")))?
        .ok_or_else(|| ApiError::not_found(format!("session not found: {session_id}")))?;
    Ok(Json(serde_json::to_value(s).unwrap_or(json!({}))))
}

pub(crate) async fn delete_chat_session(
    State(state): State<AppState>,
    AxumPath((id, session_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    store
        .delete_session(&session_id)
        .map_err(|e| ApiError::internal(format!("delete session: {e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}
