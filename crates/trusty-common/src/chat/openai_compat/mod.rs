//! OpenAI-compatible SSE streaming providers: OpenRouter and Ollama.
//!
//! Why: OpenRouter and Ollama both speak the same OpenAI-compatible
//! `/v1/chat/completions` wire format with SSE streaming. Keeping the shared
//! SSE pump and both providers in one module avoids duplication while keeping
//! the Bedrock provider isolated in its own module.
//! What: [`OpenRouterProvider`], [`OllamaProvider`], the shared SSE pump
//! (`pump_openai_sse`), and [`auto_detect_local_provider`] which probes a
//! running local server.
//! Test: `ollama_provider_streams_sse_deltas`, `auto_detect_returns_none_on_unreachable`,
//! `accumulates_streamed_tool_call_fragments`, etc. in the test suite below.

mod providers;
mod sse_pump;
mod wire;

pub use providers::{OllamaProvider, OpenRouterProvider, auto_detect_local_provider};

#[cfg(test)]
mod tests {
    use super::providers::{OllamaProvider, OpenRouterProvider, auto_detect_local_provider};
    use super::sse_pump::ToolCallAccumulator;
    use super::wire::tools_wire;
    use crate::chat::{ChatEvent, ChatProvider, ToolDef};

    #[test]
    fn openrouter_provider_reports_metadata() {
        let p = OpenRouterProvider::new("sk-xxx", "anthropic/claude-3.5-sonnet");
        assert_eq!(p.name(), "openrouter");
        assert_eq!(p.model(), "anthropic/claude-3.5-sonnet");
    }

    #[test]
    fn ollama_provider_reports_metadata() {
        let p = OllamaProvider::new("http://localhost:11434", "llama3.2");
        assert_eq!(p.name(), "ollama");
        assert_eq!(p.model(), "llama3.2");
    }

    #[test]
    fn tool_def_serializes_as_function() {
        let tools = vec![ToolDef {
            name: "search".into(),
            description: "Search the web".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
            }),
        }];
        let wire = tools_wire(&tools).expect("expected Some");
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v[0]["type"], "function");
        assert_eq!(v[0]["function"]["name"], "search");
        assert_eq!(v[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn empty_tools_serializes_to_none() {
        assert!(tools_wire(&[]).is_none());
    }

    #[test]
    fn accumulates_streamed_tool_call_fragments() {
        let mut acc = ToolCallAccumulator::default();
        acc.apply_delta(&serde_json::json!([{
            "index": 0,
            "id": "call_abc",
            "function": { "name": "search", "arguments": "" }
        }]));
        acc.apply_delta(&serde_json::json!([{
            "index": 0,
            "function": { "arguments": "{\"query\":\"" }
        }]));
        acc.apply_delta(&serde_json::json!([{
            "index": 0,
            "function": { "arguments": "rust\"}" }
        }]));
        let calls = acc.finalize();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].arguments, "{\"query\":\"rust\"}");
    }

    #[tokio::test]
    async fn auto_detect_returns_none_on_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let base = format!("http://127.0.0.1:{port}");
        let start = std::time::Instant::now();
        let got = auto_detect_local_provider(&base).await;
        let elapsed = start.elapsed();
        assert!(got.is_none(), "expected None for unreachable server");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "auto-detect took too long: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn auto_detect_returns_some_on_200() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let body = b"{\"data\":[]}";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.shutdown().await;
            }
        });

        let got = auto_detect_local_provider(&base).await;
        assert!(got.is_some(), "expected Some for reachable 200 server");
        let p = got.unwrap();
        assert_eq!(p.name(), "ollama");
        assert_eq!(p.base_url, base);
    }

    #[tokio::test]
    async fn ollama_provider_streams_sse_deltas() {
        use crate::chat::ChatProvider;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;

                let sse_body = concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hello \"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\n\n",
                    "data: [DONE]\n\n",
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    sse_body.len(),
                    sse_body
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let provider = OllamaProvider::new(base, "test-model");
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);
        let handle = tokio::spawn(async move {
            provider
                .chat_stream(
                    vec![crate::ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                        tool_call_id: None,
                        tool_calls: None,
                    }],
                    vec![],
                    tx,
                )
                .await
        });

        let mut deltas = Vec::new();
        let mut saw_done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                ChatEvent::Delta(s) => deltas.push(s),
                ChatEvent::Done => saw_done = true,
                ChatEvent::ToolCall(_) => panic!("unexpected tool call"),
                ChatEvent::Error(e) => panic!("stream error: {e}"),
            }
        }
        let result = handle.await.expect("task panicked");
        assert!(result.is_ok(), "chat_stream errored: {result:?}");
        assert_eq!(deltas, vec!["hello ".to_string(), "world".to_string()]);
        assert!(saw_done, "expected ChatEvent::Done");
    }

    #[tokio::test]
    async fn ollama_provider_emits_tool_call() {
        use crate::chat::ChatProvider;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;

                let sse_body = concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search\",\"arguments\":\"{\\\"q\\\":\"}}]}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"rust\\\"}\"}}]}}]}\n\n",
                    "data: [DONE]\n\n",
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    sse_body.len(),
                    sse_body
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let provider = OllamaProvider::new(base, "test-model");
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);
        let handle = tokio::spawn(async move {
            provider
                .chat_stream(
                    vec![crate::ChatMessage {
                        role: "user".into(),
                        content: "search rust".into(),
                        tool_call_id: None,
                        tool_calls: None,
                    }],
                    vec![ToolDef {
                        name: "search".into(),
                        description: "search the web".into(),
                        parameters: serde_json::json!({"type":"object"}),
                    }],
                    tx,
                )
                .await
        });

        let mut tool_calls = Vec::new();
        let mut saw_done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                crate::chat::ChatEvent::ToolCall(tc) => tool_calls.push(tc),
                ChatEvent::Done => saw_done = true,
                ChatEvent::Delta(_) => {}
                ChatEvent::Error(e) => panic!("stream error: {e}"),
            }
        }
        let result = handle.await.expect("task panicked");
        assert!(result.is_ok(), "chat_stream errored: {result:?}");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "search");
        assert_eq!(tool_calls[0].arguments, "{\"q\":\"rust\"}");
        assert!(saw_done);
    }
}
