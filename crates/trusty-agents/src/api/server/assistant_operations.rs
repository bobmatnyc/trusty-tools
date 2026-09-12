//! Closed assistant memory and Concierge API operations (#7360/#7361).
use crate::tools::{ToolExecutor, ToolResult};
use axum::{Json, extract::Path, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
type Error = (StatusCode, Json<Value>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Remember {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
    context: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Recall {
    query: String,
    #[serde(default)]
    across_palaces: bool,
    top_k: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Concierge {
    action: String,
    #[serde(default)]
    section: String,
    #[serde(default)]
    patch: Value,
}
fn result(value: ToolResult) -> Result<Json<Value>, Error> {
    if value.is_error() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":value.content()})),
        ));
    }
    serde_json::from_str(value.content())
        .map(Json)
        .map_err(|_| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":"Invalid service response"})),
            )
        })
}
async fn memory(name: &str, operation: &'static str, args: Value) -> Result<Json<Value>, Error> {
    let config = crate::agents::AgentConfig::by_name_async(name)
        .await
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Unknown assistant"})),
            )
        })?;
    if !crate::tools::assistant_memory::operation_granted(&config, operation) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Memory permission denied"})),
        ));
    }
    result(crate::tools::assistant_memory::execute_operation(name, operation, args).await)
}
/// Why: remembering a fact must be directly testable without an LLM tool call.
/// What: invoke the same assistant-bound durable writer as memory_remember.
/// Test: `direct_operations_reject_untrusted_fields` and `memory_write_wait_observes_permission_revocation`.
pub(super) async fn remember(
    Path(name): Path<String>,
    request: Result<Json<Remember>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(request) = request.map_err(super::attachment_prepare::json_error)?;
    remember_operation(&name, request, "memory_remember").await
}
/// Why: the write alias must honor its own tool grant while sharing durable semantics.
/// What: validate through the remember service and invoke the bound memory_write alias.
/// Test: `direct_operations_reject_untrusted_fields`.
pub(super) async fn write(
    Path(name): Path<String>,
    request: Result<Json<Remember>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(request) = request.map_err(super::attachment_prepare::json_error)?;
    remember_operation(&name, request, "memory_write").await
}
async fn remember_operation(
    name: &str,
    request: Remember,
    operation: &'static str,
) -> Result<Json<Value>, Error> {
    if request.text.trim().is_empty()
        || request.text.len() > 64 * 1024
        || request.tags.len() > 32
        || request.tags.iter().any(|t| t.len() > 128)
        || request
            .context
            .as_ref()
            .is_some_and(|s| s.len() > 16 * 1024)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Invalid fact text, tags or context"})),
        ));
    }
    memory(
        name,
        operation,
        json!({"text":request.text,"tags":request.tags,"context":request.context}),
    )
    .await
}

/// Why: fact recall must be testable without an LLM and preserve assistant read policy.
/// What: validate bounded queries and invoke the same bound recall service as tools.
/// Test: `direct_operations_reject_untrusted_fields`.
pub(super) async fn recall(
    Path(name): Path<String>,
    request: Result<Json<Recall>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(request) = request.map_err(super::attachment_prepare::json_error)?;
    if request.query.trim().is_empty()
        || request.query.len() > 16 * 1024
        || request.top_k.is_some_and(|k| !(1..=50).contains(&k))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Invalid recall query or top_k"})),
        ));
    }
    memory(&name,"memory_recall",json!({"query":request.query,"across_palaces":request.across_palaces,"top_k":request.top_k.unwrap_or(6)})).await
}
/// Why: API and assistant configuration must share the same validated operations.
/// What: use router-authenticated operator authority and a self-bound target, never request privilege flags.
/// Test: `direct_operations_reject_untrusted_fields`.
pub(super) async fn concierge(
    Path(name): Path<String>,
    request: Result<Json<Concierge>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(request) = request.map_err(super::attachment_prepare::json_error)?;
    result(
        crate::tools::concierge::ConciergeTool::assistant(&name, false)
            .execute(
                json!({"action":request.action,"section":request.section,"patch":request.patch}),
            )
            .await,
    )
}

/// Why: image history needs authenticated reads without accepting client filesystem paths.
/// What: require a generated ID and assistant/session ownership; return validated bytes or a structured error.
/// Test: `direct_operations_reject_untrusted_fields`; persistence ownership is covered by trusty-common asset_ownership_and_restart_are_enforced.
pub(super) async fn asset(Path((name, id)): Path<(String, String)>) -> axum::response::Response {
    use axum::response::IntoResponse;
    if uuid::Uuid::parse_str(&id).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Invalid asset ID"})),
        )
            .into_response();
    }
    match crate::chat_attachments::get_asset(&name, &id).await {
        Ok((mime, bytes)) => (
            [
                (axum::http::header::CONTENT_TYPE, mime),
                (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
                (
                    axum::http::header::CACHE_CONTROL,
                    "private, no-store".into(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error":e.to_string()}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn direct_operations_reject_untrusted_fields() {
        let router = Router::new()
            .route("/{name}/remember", axum::routing::post(remember))
            .route("/{name}/recall", axum::routing::post(recall))
            .route("/{name}/concierge", axum::routing::post(concierge))
            .route("/{name}/assets/{id}", axum::routing::get(asset));
        for (route, body) in [
            ("remember", json!({"text":"Blue","palace":"foreign"})),
            ("remember", json!({"text":"Blue","allow_secret_like":true})),
            ("recall", json!({"query":"Blue","namespace":"foreign"})),
            (
                "concierge",
                json!({"action":"settings.patch","read_only":false}),
            ),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::post(format!("/fixture/{route}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let value: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(value["error"].is_string());
        }
        let response = router
            .oneshot(
                Request::get("/fixture/assets/not-an-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
