//! The `search.chat` core: answer one question grounded in an index.
//!
//! Why: the socket method `search.chat` (`service::rpc::chat`) is the one door
//! to the chat lane. #9214 removed the TCP listener and #9535 deleted the `/ui`
//! handlers, the `include_dir!` embed of `ui-dist/`, `POST /chat` and
//! `GET /api/chat/providers`; the search dashboard is served by trusty-console.
//!
//! What: the request types and [`chat_report`], which resolves the provider,
//! gathers search context and collects the streamed reply into one envelope.
//!
//! Test: `chat_without_a_provider_is_refused_identically_on_both_transports`.

use serde::Deserialize;
use std::sync::Arc;

use axum::http::StatusCode;

use crate::service::server::SearchAppState;
use trusty_common::{ChatEvent, ChatMessage as CommonChatMessage};

/// Inbound payload for `search.chat`.
///
/// Why: The browser doesn't see the OpenRouter API key — the daemon proxies
/// the request server-side using `OPENROUTER_API_KEY` from the environment.
/// Programmatic callers (MCP, CLI) may instead supply `api_key` in the body.
/// What: Caller supplies `index_id` (the collection to ground the question
/// in), the new `message` (or `question`), optional prior `history`,
/// optional `model` (default `anthropic/claude-haiku-4.5`), optional `top_k`
/// (default 5), and optional `api_key`. [`chat_report`] runs a search to
/// gather context, then forwards a chat completion request to the provider.
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

/// The answer `search.chat` serves (#6285 slice 5.6).
///
/// Why: `search.chat` is the method the search UI in `trusty-console` dials.
/// The provider resolution is the part that must not be re-implemented: it
/// decides between the auto-detected local provider, a per-request `api_key`,
/// and the daemon's env-configured OpenRouter key.
///
/// What: resolves the provider, searches `index_id` for grounding context,
/// streams the completion, and collects the deltas into one envelope —
/// `{reply, answer, sources, model, provider}`. The stream is internal: the
/// socket answers one frame (#6285 slice 5's streaming pair is reindex progress
/// and the daemon status feed, not this).
///
/// # Errors
///
/// `400` when neither `message` nor `question` carried text; `503` when no
/// provider is configured at all; `502` when the provider failed or the stream
/// reported an error; `500` when the collector task could not be joined. The
/// status becomes the JSON-RPC code. A search that fails is NOT an error — the
/// question is answered without grounding context.
///
/// Test: `an_empty_message_is_refused_identically_on_both_transports`,
/// `chat_without_a_provider_is_refused_identically_on_both_transports`,
/// `the_question_alias_decodes_on_both_transports`.
pub(crate) async fn chat_report(
    state: &Arc<SearchAppState>,
    req: ChatRequest,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    if req.message.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "message (or question) is required"}),
        ));
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
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    serde_json::json!({
                        "error": "no chat provider available: start a local model server (Ollama / LM Studio) or set OPENROUTER_API_KEY",
                    }),
                ));
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

    // 3. Stream from the provider and collect deltas. The answer stays a
    //    single JSON envelope (matching the prior `openrouter_chat` contract);
    //    the streaming abstraction is internal.
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
            // #3767: no usage-accounting consumer wired up here yet; ignore
            // rather than let the match go non-exhaustive.
            ChatEvent::Usage(_) => {}
            ChatEvent::Done => {}
            ChatEvent::Error(e) => stream_error = Some(e),
            // `ChatEvent` is `#[non_exhaustive]` (trusty-common 0.27.0): a wildcard
            // keeps a future variant from breaking this crate's build.
            _ => {}
        }
    }
    match stream_task.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                serde_json::json!({"error": format!("{}: {e}", provider.name())}),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": format!("chat task join: {e}")}),
            ));
        }
    }
    if let Some(e) = stream_error {
        return Err((
            StatusCode::BAD_GATEWAY,
            serde_json::json!({"error": format!("{}: {e}", provider.name())}),
        ));
    }

    let model = provider.model().to_string();
    Ok(serde_json::json!({
        "reply": reply,
        "answer": reply,
        "sources": sources,
        "model": model,
        "provider": provider.name(),
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
        path_prefix: None,
        repos: Vec::new(),
        ripgrep_fallback: None,
        lexical_limit: None,
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
