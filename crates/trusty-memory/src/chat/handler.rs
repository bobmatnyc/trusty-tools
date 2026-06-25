//! The SSE-streaming chat handler (`chat_handler`).
//!
//! Why: the OpenRouter/Ollama tool-calling loop is by far the largest single
//! concern in the chat HTTP surface; isolating it keeps the other handlers
//! readable (split out of the former monolithic `chat.rs`, issue #607).
//! What: the `chat_handler` axum handler, moved verbatim. Tool-loop building
//! blocks (`all_tools`, `execute_tool`, `MAX_TOOL_ROUNDS`, `ChatBody`) are
//! pulled in from the sibling `tools` submodule.
//! Test: behaviour exercised end-to-end via the chat SSE integration paths.

use crate::web::load_user_config;
use crate::AppState;
use axum::{
    body::Body,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::recall_with_default_embedder;
use trusty_common::memory_core::PalaceRegistry;
use trusty_common::{ChatEvent, ChatMessage};

// ---------------------------------------------------------------------------

use super::tools::{all_tools, execute_get_dream_status, execute_tool, ChatBody, MAX_TOOL_ROUNDS};

pub(crate) async fn chat_handler(
    State(state): State<AppState>,
    Json(body): Json<ChatBody>,
) -> Response {
    // Select the active provider (Ollama auto-detect, else OpenRouter).
    let Some(provider) = state.chat_provider().await else {
        return (
            StatusCode::PRECONDITION_FAILED,
            "No chat provider configured (no local Ollama detected and no OpenRouter key set)",
        )
            .into_response();
    };

    // Resolve palace id (explicit > default).
    let palace_id = body
        .palace_id
        .clone()
        .or_else(|| state.default_palace.clone())
        .unwrap_or_default();

    // Resolve / create chat session when a palace is bound.
    let (session_id, mut history): (Option<String>, Vec<ChatMessage>) = if !palace_id.is_empty() {
        let store = match state.session_store(&palace_id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(palace = %palace_id, "session_store open failed: {e:#}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("session store: {e:#}"),
                )
                    .into_response();
            }
        };
        match body.session_id.clone() {
            Some(sid) => match store.get_session(&sid) {
                Ok(Some(s)) => (
                    Some(sid),
                    s.history
                        .into_iter()
                        .map(|m| ChatMessage {
                            role: m.role,
                            content: m.content,
                            tool_call_id: None,
                            tool_calls: None,
                        })
                        .collect(),
                ),
                _ => (Some(sid), body.history.clone()),
            },
            None => {
                let new_id = store.create_session(None).unwrap_or_else(|e| {
                    tracing::warn!("create_session failed: {e:#}");
                    String::new()
                });
                (
                    if new_id.is_empty() {
                        None
                    } else {
                        Some(new_id)
                    },
                    body.history.clone(),
                )
            }
        }
    } else {
        (None, body.history.clone())
    };

    // Full palace roster for the identity block — names + ids, not just count,
    // so the model can pick the right one when the user names a palace.
    let all_palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let palace_count = all_palaces.len();
    let palace_roster: String = all_palaces
        .iter()
        .map(|p| format!("- {} (id: {})", p.name, p.id.0))
        .collect::<Vec<_>>()
        .join("\n");

    // Config + global dream snapshot — give the model an honest view of what's
    // available so it doesn't invent tools or providers that aren't there.
    let cfg = load_user_config().unwrap_or_default();
    let active_provider_name = state
        .chat_provider()
        .await
        .map(|p| p.name().to_string())
        .unwrap_or_else(|| "none".to_string());
    let dream_snapshot = execute_get_dream_status(&state).await;

    // Look up the selected palace's metadata (name/description) and open its
    // handle for live counts + recall context.
    let selected_palace_meta = if palace_id.is_empty() {
        None
    } else {
        all_palaces.iter().find(|p| p.id.0 == palace_id).cloned()
    };

    let mut palace_block = String::new();
    let mut context = String::new();
    let mut palace_display_name = palace_id.clone();

    if !palace_id.is_empty() {
        if let Ok(handle) = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new(&palace_id))
        {
            // Live counts from the opened handle.
            let drawer_count = handle.drawers.read().len();
            let vector_count = handle.vector_store.index_size();
            let kg_triple_count = handle.kg.count_active_triples();

            // Prefer the on-disk palace.json name/description; fall back to id.
            let (name, description) = match &selected_palace_meta {
                Some(p) => (p.name.clone(), p.description.clone()),
                None => (palace_id.clone(), None),
            };
            palace_display_name = name.clone();

            palace_block.push_str(&format!(
                "Currently selected palace:\n\
                 - id: {id}\n\
                 - name: {name}\n",
                id = palace_id,
                name = name,
            ));
            if let Some(desc) = description.as_deref().filter(|s| !s.is_empty()) {
                palace_block.push_str(&format!("- description: {desc}\n"));
            }
            palace_block.push_str(&format!(
                "- drawers: {drawer_count}\n\
                 - vectors: {vector_count}\n\
                 - kg_triples: {kg_triple_count}\n",
            ));
            let identity_trimmed = handle.identity.trim();
            if !identity_trimmed.is_empty() {
                palace_block.push_str(&format!("- identity:\n{identity_trimmed}\n",));
            }

            if let Ok(hits) = recall_with_default_embedder(&handle, &body.message, 5).await {
                for r in hits.iter().take(5) {
                    context.push_str(&format!("- (L{}) {}\n", r.layer, r.drawer.content));
                }
            }
        }
    }

    // Build the grounded system prompt with identity, palace, RAG, config,
    // dream-snapshot, and behavior blocks so the LLM never confuses
    // trusty-memory palaces with real-world architectural palaces.
    let mut system = String::new();
    system.push_str(&format!(
        "You are the assistant for trusty-memory, a machine-wide AI memory \
         service running locally on this user's machine. trusty-memory stores \
         knowledge in named \"palaces\" — isolated memory namespaces, each with \
         its own vector index (usearch HNSW) and temporal knowledge graph \
         (redb). Memories are organized as Palace -> Wing -> Room -> Closet \
         -> Drawer, where a Drawer is an atomic memory unit.\n\
         There are currently {palace_count} palace(s) on this machine.\n",
    ));
    if !palace_roster.is_empty() {
        system.push_str(&format!("Palaces:\n{palace_roster}\n"));
    }
    system.push('\n');

    // Config block — what providers/models are wired up right now.
    system.push_str(&format!(
        "System configuration:\n\
         - active chat provider: {active_provider_name}\n\
         - openrouter model: {or_model}\n\
         - local model: {local_model} ({local_url}, enabled={local_enabled})\n\
         - data root: {data_root}\n\n",
        or_model = cfg.openrouter_model,
        local_model = cfg.local_model.model,
        local_url = cfg.local_model.base_url,
        local_enabled = cfg.local_model.enabled,
        data_root = state.data_root.display(),
    ));

    // Dream snapshot — give the model a sense of how stale memory state is.
    system.push_str(&format!(
        "Global dream status (background memory maintenance):\n{}\n\n",
        dream_snapshot,
    ));

    if !palace_block.is_empty() {
        system.push_str(&palace_block);
        system.push('\n');
    }

    if !context.is_empty() {
        system.push_str(&format!(
            "Relevant memories from the '{palace_display_name}' palace \
             (L0 = identity, L1 = essentials, L2 = topic-filtered, L3 = deep):\n\
             {context}\n",
        ));
    }

    system.push_str(
        "You have a set of tools to introspect and modify this trusty-memory \
         daemon. Prefer calling a tool over guessing — e.g. call \
         `list_palaces` rather than relying on the roster above if you need \
         live counts, and call `recall_memories` to search for facts you \
         don't have in context. When the user asks about \"palaces\", they \
         mean trusty-memory palaces (memory namespaces on this machine), not \
         architectural palaces like Versailles. If a tool returns an error, \
         report it honestly and don't fabricate results.",
    );

    // Append the new user message to the in-memory history we'll persist.
    history.push(ChatMessage {
        role: "user".to_string(),
        content: body.message.clone(),
        tool_call_id: None,
        tool_calls: None,
    });

    let mut messages: Vec<ChatMessage> = Vec::with_capacity(history.len() + 1);
    messages.push(ChatMessage {
        role: "system".to_string(),
        content: system,
        tool_call_id: None,
        tool_calls: None,
    });
    messages.extend(history.iter().cloned());

    let tools = all_tools();
    let (sse_tx, sse_rx) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(64);

    // Capture session-persistence inputs.
    let session_store = if !palace_id.is_empty() && session_id.is_some() {
        state.session_store(&palace_id).ok()
    } else {
        None
    };
    let persist_session_id = session_id.clone();

    // Drive the tool-execution loop in a background task so the response can
    // start streaming immediately.
    let loop_state = state.clone();
    tokio::spawn(async move {
        // Emit a leading session_id frame so the SPA can correlate this stream
        // with a persisted session row.
        if let Some(sid) = persist_session_id.as_deref() {
            let frame = format!("data: {}\n\n", json!({ "session_id": sid }));
            if sse_tx
                .send(Ok(axum::body::Bytes::from(frame)))
                .await
                .is_err()
            {
                return;
            }
        }

        let mut final_assistant_text = String::new();
        let mut stream_err: Option<String> = None;

        for round in 0..MAX_TOOL_ROUNDS {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ChatEvent>(256);
            let messages_clone = messages.clone();
            let tools_clone = tools.clone();
            let provider_clone = provider.clone();
            let stream_handle = tokio::spawn(async move {
                provider_clone
                    .chat_stream(messages_clone, tools_clone, event_tx)
                    .await
            });

            let mut tool_calls_this_round: Vec<trusty_common::ToolCall> = Vec::new();
            let mut round_assistant_text = String::new();

            while let Some(event) = event_rx.recv().await {
                match event {
                    ChatEvent::Delta(text) => {
                        round_assistant_text.push_str(&text);
                        let frame = format!("data: {}\n\n", json!({ "delta": text }));
                        if sse_tx
                            .send(Ok(axum::body::Bytes::from(frame)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ChatEvent::ToolCall(tc) => {
                        let frame = format!(
                            "data: {}\n\n",
                            json!({ "tool_call": {
                                "id": tc.id,
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }})
                        );
                        let _ = sse_tx.send(Ok(axum::body::Bytes::from(frame))).await;
                        tool_calls_this_round.push(tc);
                    }
                    ChatEvent::Done => break,
                    ChatEvent::Error(e) => {
                        stream_err = Some(e);
                        break;
                    }
                }
            }

            // Drain the spawned stream task; surface any error.
            match stream_handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => stream_err = Some(e.to_string()),
                Err(e) => stream_err = Some(format!("join: {e}")),
            }

            if stream_err.is_some() {
                break;
            }

            final_assistant_text.push_str(&round_assistant_text);

            if tool_calls_this_round.is_empty() {
                // Model produced a plain answer — we're done.
                break;
            }

            // Build the assistant message that requested these tool calls.
            let assistant_tool_calls_json: Vec<Value> = tool_calls_this_round
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments },
                    })
                })
                .collect();
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: round_assistant_text,
                tool_call_id: None,
                tool_calls: Some(assistant_tool_calls_json),
            });

            // Execute each tool and append its result as a `role: "tool"`
            // message. The next loop iteration feeds these back to the model.
            for tc in &tool_calls_this_round {
                let result = execute_tool(&tc.name, &tc.arguments, &loop_state).await;
                let result_str = result.to_string();
                let frame = format!(
                    "data: {}\n\n",
                    json!({ "tool_result": {
                        "id": tc.id,
                        "name": tc.name,
                        "content": &result_str,
                    }})
                );
                let _ = sse_tx.send(Ok(axum::body::Bytes::from(frame))).await;
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result_str,
                    tool_call_id: Some(tc.id.clone()),
                    tool_calls: None,
                });
            }

            // Safety net: log when we walk off the round limit.
            if round + 1 == MAX_TOOL_ROUNDS {
                tracing::warn!(
                    "chat: hit MAX_TOOL_ROUNDS={} — terminating tool loop",
                    MAX_TOOL_ROUNDS
                );
            }
        }

        // Persist the completed conversation regardless of streaming error
        // (partial assistant reply still better than nothing).
        if let (Some(store), Some(sid)) = (session_store, persist_session_id.as_deref()) {
            if !final_assistant_text.is_empty() {
                history.push(ChatMessage {
                    role: "assistant".into(),
                    content: final_assistant_text,
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            let core_history: Vec<trusty_common::memory_core::store::chat_sessions::ChatMessage> =
                history
                    .iter()
                    .map(
                        |m| trusty_common::memory_core::store::chat_sessions::ChatMessage {
                            role: m.role.clone(),
                            content: m.content.clone(),
                        },
                    )
                    .collect();
            if let Err(e) = store.upsert_session(sid, &core_history) {
                tracing::warn!("upsert_session failed: {e:#}");
            }
        }

        match stream_err {
            None => {
                let _ = sse_tx
                    .send(Ok(axum::body::Bytes::from("data: [DONE]\n\n")))
                    .await;
            }
            Some(e) => {
                let out = format!("data: {}\n\n", json!({ "error": e }));
                let _ = sse_tx.send(Ok(axum::body::Bytes::from(out))).await;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);

    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(stream))
        .expect("static SSE response builds")
}
