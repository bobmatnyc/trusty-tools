//! Embedded Svelte UI server.
//!
//! Why: We ship a single `trusty-search` binary that serves the management
//! UI without requiring users to run a separate static-file server. The
//! Svelte build output (`ui/dist/`) is baked into the binary at compile time
//! via `include_dir!`, so the daemon is fully self-contained.
//!
//! What: Two route handlers serving the SPA:
//!   - `GET /ui`       → index.html with runtime config injected
//!   - `GET /ui/*path` → static asset, falling back to index.html for
//!     client-side routes (e.g. `/ui/search`).
//!
//! Plus the OpenRouter-proxying `POST /chat` endpoint.
//!
//! Test: `cargo test -p trusty-search-service ui::` exercises the path
//! resolver against the embedded directory.

use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use std::sync::Arc;

use crate::service::server::SearchAppState;
use trusty_common::{ChatEvent, ChatMessage as CommonChatMessage};

/// Why: `include_dir!` walks at compile time and embeds every byte. We point
/// it at `ui-dist/` inside this crate's directory (committed alongside the
/// source so it is available during `cargo publish` packaging).
/// What: `UI_DIR` is a static reference to the compiled tree.
/// Test: `cargo build` produces a binary that, when run, serves `/ui` with
/// the SPA shell. To regenerate: `npm run build` in `ui/`, then
/// `cp -r ui/dist crates/trusty-search-service/ui-dist`.
static UI_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui-dist");

/// Inject runtime configuration into index.html before serving.
///
/// Why: The browser needs to know (a) the daemon port (so it can reach
/// the API at the right host:port even when the UI is opened directly via
/// `file://` for local dev), and (b) whether the OpenRouter chat lane is
/// enabled. We can't bake these in at compile time because they're chosen
/// at runtime.
/// What: Replaces the placeholder boot script with one that sets both
/// globals before the bundle loads.
/// Test: After serving, `view-source:` shows the correct port literal.
fn inject_runtime_config(html: &str, port: u16, openrouter_enabled: bool) -> String {
    let inject = format!(
        "<script>\n\
         window.__DAEMON_PORT__ = {};\n\
         window.__OPENROUTER_ENABLED__ = {};\n\
         </script>",
        port,
        if openrouter_enabled { "true" } else { "false" }
    );
    // Insert just before </head> so the inline script runs before the
    // bundle. If </head> isn't found (shouldn't happen with vite output),
    // prepend to keep behavior safe.
    if let Some(idx) = html.find("</head>") {
        let mut out = String::with_capacity(html.len() + inject.len());
        out.push_str(&html[..idx]);
        out.push_str(&inject);
        out.push_str(&html[idx..]);
        out
    } else {
        format!("{inject}{html}")
    }
}

/// Serve `index.html` at `/ui`.
pub async fn ui_index_handler(State(state): State<Arc<SearchAppState>>) -> Response {
    serve_index(&state).await
}

/// Serve any file under `/ui/*path`, falling back to index.html for SPA
/// routes that don't map to a real file.
pub async fn ui_asset_handler(
    State(state): State<Arc<SearchAppState>>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    // Strip leading slashes — include_dir paths are relative.
    let trimmed = path.trim_start_matches('/');
    if let Some(file) = UI_DIR.get_file(trimmed) {
        let mime = mime_for(trimmed);
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, cache_control_for(trimmed))
            .body(Body::from(file.contents()))
            .expect("response builder fields are all valid");
    }
    // SPA fallback.
    serve_index(&state).await
}

async fn serve_index(state: &SearchAppState) -> Response {
    let Some(index_file) = UI_DIR.get_file("index.html") else {
        return (
            StatusCode::NOT_FOUND,
            "UI assets not bundled — run `npm run build` in ui/ before `cargo build`.",
        )
            .into_response();
    };
    let html_bytes = index_file.contents();
    let html = std::str::from_utf8(html_bytes).unwrap_or_default();
    let port = state.daemon_port.unwrap_or(crate::service::DEFAULT_PORT);
    let body = inject_runtime_config(html, port, state.openrouter_enabled);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .expect("response builder fields are all valid")
}

fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

fn cache_control_for(path: &str) -> &'static str {
    // Vite hashes asset filenames, so /assets/* is safe to cache aggressively.
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

// ── Chat endpoint ──────────────────────────────────────────────────────────

/// Inbound payload for `POST /chat`.
///
/// Why: The browser doesn't see the OpenRouter API key — the daemon proxies
/// the request server-side using `OPENROUTER_API_KEY` from the environment.
/// Programmatic callers (MCP, CLI) may instead supply `api_key` in the body.
/// What: Caller supplies `index_id` (the collection to ground the question
/// in), the new `message` (or `question`), optional prior `history`,
/// optional `model` (default `anthropic/claude-haiku-4.5`), optional `top_k`
/// (default 5), and optional `api_key`. The handler runs a search to gather
/// context, then forwards a chat completion request to OpenRouter.
/// Test: With `OPENROUTER_API_KEY` unset and no `api_key` in body →
/// returns 503 + `{error}`. With both → uses env var.
#[derive(Deserialize)]
pub struct ChatRequest {
    pub index_id: String,
    /// Primary user message. Accept either `message` (browser UI) or
    /// `question` (issue #15 spec) — they're aliases.
    #[serde(default, alias = "question")]
    pub message: String,
    #[serde(default)]
    pub history: Vec<ChatMessage>,
    /// OpenRouter model id. Defaults to `anthropic/claude-haiku-4.5`.
    #[serde(default)]
    pub model: Option<String>,
    /// Number of context chunks to retrieve. Defaults to 5.
    #[serde(default)]
    pub top_k: Option<usize>,
    /// Fallback API key when `OPENROUTER_API_KEY` env var is not set.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Default OpenRouter model when caller doesn't specify one.
///
/// Why: Centralize the default so MCP, HTTP, and CLI all agree.
/// What: Returns the model id literal.
/// Test: `assert_eq!(default_model(), "anthropic/claude-haiku-4.5")`.
pub fn default_model() -> &'static str {
    "anthropic/claude-haiku-4.5"
}

#[derive(Deserialize, serde::Serialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub async fn chat_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(req): Json<ChatRequest>,
) -> Response {
    if req.message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "message (or question) is required"})),
        )
            .into_response();
    }

    let top_k = req.top_k.unwrap_or(5).max(1);

    // Resolve the active provider (Ollama if detected, else OpenRouter). If the
    // request body supplies an `api_key` AND the cached provider is unavailable
    // or non-OpenRouter, we build a one-shot OpenRouter provider so scripted
    // callers can override env-driven config (issue #15).
    let provider: Arc<dyn trusty_common::ChatProvider> = match state.chat_provider().await {
        Some(p) if req.api_key.as_ref().is_none_or(|k| k.is_empty()) => p,
        _ => {
            // Either no auto-detected provider, or caller supplied an api_key.
            // Prefer the explicit api_key when provided.
            let api_key = req.api_key.clone().filter(|k| !k.is_empty()).or_else(|| {
                if state.openrouter_api_key.is_empty() {
                    None
                } else {
                    Some(state.openrouter_api_key.clone())
                }
            });
            let Some(api_key) = api_key else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": "no chat provider available: start a local model server (Ollama / LM Studio) or set OPENROUTER_API_KEY",
                    })),
                )
                    .into_response();
            };
            let model = req
                .model
                .clone()
                .unwrap_or_else(|| state.openrouter_model.clone());
            Arc::new(trusty_common::OpenRouterProvider::new(api_key, model))
        }
    };

    // 1. Search the index for context (best-effort — empty context is fine).
    let (context_snippet, sources) =
        match search_for_context(state.as_ref(), &req.index_id, &req.message, top_k).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("chat: search for context failed: {e}");
                (String::new(), Vec::new())
            }
        };

    // 2. Build messages: system prompt with context, then history, then new user message.
    let system = format!(
        "You are a code-aware assistant for the '{}' codebase. \
         Answer the user's question using the search results below as primary context. \
         If the context doesn't cover the question, say so honestly.\n\n\
         === Search Context ===\n{}\n=== End Context ===",
        req.index_id, context_snippet
    );

    let mut messages: Vec<CommonChatMessage> = Vec::new();
    messages.push(CommonChatMessage {
        role: "system".into(),
        content: system,
        tool_call_id: None,
        tool_calls: None,
    });
    for m in &req.history {
        messages.push(CommonChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_call_id: None,
            tool_calls: None,
        });
    }
    messages.push(CommonChatMessage {
        role: "user".into(),
        content: req.message.clone(),
        tool_call_id: None,
        tool_calls: None,
    });

    // 3. Stream from the provider and collect deltas. The HTTP response stays
    //    a single JSON envelope (matching the prior `openrouter_chat`
    //    contract) — the streaming abstraction is internal. SSE streaming to
    //    the browser can be added later without breaking existing callers.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(64);
    let provider_for_task = provider.clone();
    let stream_task =
        tokio::spawn(async move { provider_for_task.chat_stream(messages, vec![], tx).await });

    let mut reply = String::new();
    let mut stream_error: Option<String> = None;
    while let Some(ev) = rx.recv().await {
        match ev {
            ChatEvent::Delta(d) => reply.push_str(&d),
            // No tools wired up for trusty-search yet — model shouldn't call
            // any since `tools` is empty, but if it does we ignore the call.
            ChatEvent::ToolCall(_) => {}
            ChatEvent::Done => {}
            ChatEvent::Error(e) => stream_error = Some(e),
        }
    }
    match stream_task.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("{}: {e}", provider.name())})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("chat task join: {e}")})),
            )
                .into_response();
        }
    }
    if let Some(e) = stream_error {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("{}: {e}", provider.name())})),
        )
            .into_response();
    }

    let model = provider.model().to_string();
    Json(serde_json::json!({
        "reply": reply,
        "answer": reply,
        "sources": sources,
        "model": model,
        "provider": provider.name(),
    }))
    .into_response()
}

/// GET /api/chat/providers — report provider availability + active choice.
///
/// Why: lets the Svelte UI (and CLI/MCP callers) show/hide the chat panel
/// based on what's actually configured server-side. Mirrors the trusty-memory
/// `/api/v1/chat/providers` shape so consumers can share UI code.
/// What: probes Ollama (1s timeout) when `local_model.enabled`, checks for a
/// non-empty OpenRouter key, and reports the active provider (lazily
/// initialised — first call resolves it for the rest of the daemon's life).
/// Test: integration tests build a fresh `SearchAppState` with neither
/// provider configured and assert the endpoint returns a 200 + well-shaped
/// JSON with `active: null`.
pub async fn list_chat_providers(
    State(state): State<Arc<SearchAppState>>,
) -> Json<serde_json::Value> {
    let ollama_available = if state.local_model.enabled {
        trusty_common::auto_detect_local_provider(&state.local_model.base_url)
            .await
            .is_some()
    } else {
        false
    };
    let openrouter_available = !state.openrouter_api_key.is_empty();
    let active = state.chat_provider().await.map(|p| p.name().to_string());
    Json(serde_json::json!({
        "providers": [
            {
                "name": "ollama",
                "model": state.local_model.model,
                "base_url": state.local_model.base_url,
                "available": ollama_available,
            },
            {
                "name": "openrouter",
                "model": state.openrouter_model,
                "available": openrouter_available,
            }
        ],
        "active": active,
    }))
}

/// Run a hybrid search and format the results as both an LLM-ready context
/// string and a JSON-serializable list of source chunks.
///
/// Why: The chat endpoint needs the formatted context for the system prompt
/// *and* the structured chunks so callers (MCP, CLI) can cite sources.
/// What: Performs a `top_k` search on `index_id` with `query`, returns
/// `(context_string, sources_json_array)`.
/// Test: With an unknown index_id, returns Err. With a valid index, the
/// returned context contains `[File: ...]` markers and `sources.len() <= top_k`.
async fn search_for_context(
    state: &SearchAppState,
    index_id: &str,
    query: &str,
    top_k: usize,
) -> Result<(String, Vec<serde_json::Value>), String> {
    use crate::core::{indexer::SearchQuery, registry::IndexId};
    let id = IndexId::new(index_id.to_string());
    let handle = state
        .registry
        .get(&id)
        .ok_or_else(|| "index not found".to_string())?;
    let q = SearchQuery {
        text: query.to_string(),
        top_k,
        expand_graph: true,
        compact: true,
        branch_files: None,
        branch_boost: SearchQuery::default_branch_boost(),
        branch: None,
        mode: crate::core::indexer::SearchMode::default(),
        exclude_archived: false,
        stage: None,
        refine_query: None,
    };
    let indexer = handle.indexer.read().await;
    let results = indexer.search(&q).await.map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mut sources: Vec<serde_json::Value> = Vec::with_capacity(results.len());
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "\n[File: {}:{}-{}] (result {}, score {:.3})\n",
            r.file,
            r.start_line,
            r.end_line,
            i + 1,
            r.score
        ));
        let snippet = r.compact_snippet.as_deref().unwrap_or(&r.content);
        out.push_str(snippet);
        out.push('\n');
        sources.push(serde_json::json!({
            "file": r.file,
            "start_line": r.start_line,
            "end_line": r.end_line,
            "score": r.score,
            "snippet": snippet,
            "function_name": r.function_name,
            "match_reason": r.match_reason,
        }));
    }
    Ok((out, sources))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Verify the runtime config injection lands before </head> so the
    /// inline globals execute before the bundle.
    /// What: Inject into a minimal HTML and assert the script is present.
    /// Test: this test.
    #[test]
    fn inject_runtime_config_inserts_before_head_close() {
        let html = "<html><head><title>x</title></head><body></body></html>";
        let out = inject_runtime_config(html, 7878, true);
        let script_idx = out.find("__DAEMON_PORT__").expect("port global injected");
        let head_close = out.find("</head>").expect("head close present");
        assert!(script_idx < head_close, "script must be inside <head>");
        assert!(out.contains("window.__OPENROUTER_ENABLED__ = true"));
    }

    #[test]
    fn inject_runtime_config_handles_missing_head() {
        let html = "<html><body></body></html>";
        let out = inject_runtime_config(html, 1234, false);
        assert!(out.starts_with("<script>"));
        assert!(out.contains("window.__DAEMON_PORT__ = 1234"));
    }

    #[test]
    fn mime_for_known_extensions() {
        assert_eq!(mime_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("a/b.js"), "application/javascript; charset=utf-8");
        assert_eq!(mime_for("a/b.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("nope"), "application/octet-stream");
    }

    #[test]
    fn cache_control_assets_are_immutable() {
        assert!(cache_control_for("assets/x.js").contains("immutable"));
        assert_eq!(cache_control_for("index.html"), "no-cache");
    }
}
