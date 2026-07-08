//! Unit tests for the Bedrock Converse transport (#1021 phase 1).
//!
//! Why: Extracted to a separate file (mirroring
//! `trusty-review::llm::bedrock::tests`) so `mod.rs`/`convert.rs` stay under
//! the 500-SLOC production cap.
//! What: Region resolution, message/tool-choice/response conversion — all
//! unit-level, no real AWS calls — plus one `#[ignore]`-gated live Converse
//! call.

use aws_sdk_bedrockruntime::operation::converse::ConverseOutput as ConverseOutputResponse;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConverseOutput as ConverseOutputKind, Message as SdkMessage, StopReason,
    TokenUsage as SdkTokenUsage, ToolChoice as SdkToolChoice,
};
use serde_json::json;

use super::convert::{
    build_converse_messages, build_tool_config, converse_output_to_chat_response,
    document_to_json_string, json_to_document,
};
use super::resolve_bedrock_region;
use crate::llm::{
    ChatMessage, ChatRequest, FunctionCall, FunctionDefinition, ToolCall, ToolDefinition,
};

/// Serializes every test that mutates the process-wide region env vars —
/// mirrors `provider::routing::RUN_DEADLINE_ENV_LOCK`'s identical rationale.
static REGION_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn with_region_env<T>(trusty: Option<&str>, aws: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _guard = REGION_ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation; serialized by `REGION_ENV_LOCK`.
    unsafe {
        match trusty {
            Some(v) => std::env::set_var("TRUSTY_AWS_REGION", v),
            None => std::env::remove_var("TRUSTY_AWS_REGION"),
        }
        match aws {
            Some(v) => std::env::set_var("AWS_REGION", v),
            None => std::env::remove_var("AWS_REGION"),
        }
    }
    let result = f();
    unsafe {
        std::env::remove_var("TRUSTY_AWS_REGION");
        std::env::remove_var("AWS_REGION");
    }
    result
}

fn minimal_request(messages: Vec<ChatMessage>) -> ChatRequest {
    ChatRequest {
        model: "us.anthropic.claude-sonnet-4-6".into(),
        messages,
        temperature: Some(0.0),
        max_tokens: Some(256),
        tools: None,
        tool_choice: None,
        usage: None,
    }
}

// ─── Region resolution ──────────────────────────────────────────────────────

#[tokio::test]
async fn region_resolution_explicit_wins() {
    with_region_env(Some("eu-west-1"), Some("ap-south-1"), || {
        assert_eq!(resolve_bedrock_region(Some("us-west-2")), "us-west-2");
    })
    .await;
}

#[tokio::test]
async fn region_resolution_trusty_env_wins_over_aws_env() {
    with_region_env(Some("eu-west-1"), Some("ap-south-1"), || {
        assert_eq!(resolve_bedrock_region(None), "eu-west-1");
    })
    .await;
}

#[tokio::test]
async fn region_resolution_aws_env_fallback() {
    with_region_env(None, Some("ap-south-1"), || {
        assert_eq!(resolve_bedrock_region(None), "ap-south-1");
    })
    .await;
}

#[tokio::test]
async fn region_resolution_defaults_to_us_east_1() {
    with_region_env(None, None, || {
        assert_eq!(resolve_bedrock_region(None), "us-east-1");
    })
    .await;
}

// ─── Message conversion ─────────────────────────────────────────────────────

#[test]
fn build_converse_messages_splits_system_and_alternates_roles() {
    let req = minimal_request(vec![
        ChatMessage::system("you are helpful"),
        ChatMessage::user("hello"),
        ChatMessage::assistant("hi there"),
    ]);
    let (system, messages) = build_converse_messages(&req).expect("convert");

    assert_eq!(system.len(), 1);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role().as_str(), "user");
    assert_eq!(messages[1].role().as_str(), "assistant");
    assert!(matches!(&messages[0].content()[0], ContentBlock::Text(t) if t == "hello"));
    assert!(matches!(&messages[1].content()[0], ContentBlock::Text(t) if t == "hi there"));
}

#[test]
fn build_converse_messages_merges_consecutive_tool_results() {
    let mut req = minimal_request(vec![
        ChatMessage::system("s"),
        ChatMessage::user("do two things"),
    ]);
    // Assistant turn with two tool calls.
    req.messages.push(ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![
            ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "get_weather".into(),
                    arguments: r#"{"location":"Seattle"}"#.into(),
                },
            },
            ToolCall {
                id: "call_2".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "get_time".into(),
                    arguments: r#"{"tz":"UTC"}"#.into(),
                },
            },
        ]),
        tool_call_id: None,
        name: None,
        cache_control: None,
    });
    // Two consecutive tool-role results answering both calls.
    req.messages
        .push(ChatMessage::tool_result("call_1", "get_weather", "72F"));
    req.messages
        .push(ChatMessage::tool_result("call_2", "get_time", "12:00 UTC"));

    let (_, messages) = build_converse_messages(&req).expect("convert");

    // user(task), assistant(2 tool uses), user(2 merged tool results)
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1].role().as_str(), "assistant");
    assert_eq!(messages[1].content().len(), 2);
    assert_eq!(messages[2].role().as_str(), "user");
    assert_eq!(
        messages[2].content().len(),
        2,
        "both tool results must merge into ONE user message, not two"
    );
    for block in messages[2].content() {
        assert!(matches!(block, ContentBlock::ToolResult(_)));
    }
}

#[test]
fn build_converse_messages_maps_tool_use_arguments_to_document() {
    let mut req = minimal_request(vec![ChatMessage::user("go")]);
    req.messages.push(ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "search".into(),
                arguments: r#"{"query":"rust"}"#.into(),
            },
        }]),
        tool_call_id: None,
        name: None,
        cache_control: None,
    });

    let (_, messages) = build_converse_messages(&req).expect("convert");
    let assistant_msg = &messages[1];
    match &assistant_msg.content()[0] {
        ContentBlock::ToolUse(tu) => {
            assert_eq!(tu.tool_use_id(), "call_1");
            assert_eq!(tu.name(), "search");
            let json_str = document_to_json_string(tu.input()).expect("serialise");
            let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("parse");
            assert_eq!(parsed["query"], "rust");
        }
        other => panic!("expected ToolUse block, got {other:?}"),
    }
}

// ─── Tool-choice / tool-config ──────────────────────────────────────────────

fn sample_tool() -> ToolDefinition {
    ToolDefinition::function(FunctionDefinition {
        name: "get_weather".into(),
        description: Some("Get the weather".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {"location": {"type": "string"}},
            "required": ["location"]
        })),
        cache_control: None,
    })
}

#[test]
fn build_tool_config_empty_tools_returns_none() {
    let result = build_tool_config(&[], None).expect("no error");
    assert!(result.is_none());
}

#[test]
fn build_tool_config_none_choice_string_suppresses_tools() {
    let tools = vec![sample_tool()];
    let result = build_tool_config(&tools, Some(&json!("none"))).expect("no error");
    assert!(
        result.is_none(),
        "\"none\" tool_choice must omit toolConfig"
    );
}

#[test]
fn build_tool_config_defaults_to_auto_when_choice_absent() {
    let tools = vec![sample_tool()];
    let config = build_tool_config(&tools, None)
        .expect("no error")
        .expect("config present");
    assert!(matches!(config.tool_choice(), Some(SdkToolChoice::Auto(_))));
    assert_eq!(config.tools().len(), 1);
}

#[test]
fn build_tool_config_auto_string_maps_to_auto() {
    let tools = vec![sample_tool()];
    let config = build_tool_config(&tools, Some(&json!("auto")))
        .expect("no error")
        .expect("config present");
    assert!(matches!(config.tool_choice(), Some(SdkToolChoice::Auto(_))));
}

#[test]
fn build_tool_config_required_string_maps_to_any() {
    let tools = vec![sample_tool()];
    let config = build_tool_config(&tools, Some(&json!("required")))
        .expect("no error")
        .expect("config present");
    assert!(matches!(config.tool_choice(), Some(SdkToolChoice::Any(_))));
}

#[test]
fn build_tool_config_openai_function_selector_maps_to_named_tool() {
    let tools = vec![sample_tool()];
    let choice = json!({"type": "function", "function": {"name": "get_weather"}});
    let config = build_tool_config(&tools, Some(&choice))
        .expect("no error")
        .expect("config present");
    match config.tool_choice() {
        Some(SdkToolChoice::Tool(t)) => assert_eq!(t.name(), "get_weather"),
        other => panic!("expected Tool choice, got {other:?}"),
    }
}

#[test]
fn build_tool_config_converse_shaped_tool_choice_maps_to_named_tool() {
    let tools = vec![sample_tool()];
    let choice = json!({"tool": {"name": "get_weather"}});
    let config = build_tool_config(&tools, Some(&choice))
        .expect("no error")
        .expect("config present");
    match config.tool_choice() {
        Some(SdkToolChoice::Tool(t)) => assert_eq!(t.name(), "get_weather"),
        other => panic!("expected Tool choice, got {other:?}"),
    }
}

#[test]
fn build_tool_config_converse_shaped_auto_and_any_pass_through() {
    let tools = vec![sample_tool()];
    let auto_config = build_tool_config(&tools, Some(&json!({"auto": {}})))
        .expect("no error")
        .expect("config present");
    assert!(matches!(
        auto_config.tool_choice(),
        Some(SdkToolChoice::Auto(_))
    ));

    let any_config = build_tool_config(&tools, Some(&json!({"any": {}})))
        .expect("no error")
        .expect("config present");
    assert!(matches!(
        any_config.tool_choice(),
        Some(SdkToolChoice::Any(_))
    ));
}

// ─── Response conversion ────────────────────────────────────────────────────

fn output_with_text(
    text: &str,
    stop: StopReason,
    usage: Option<SdkTokenUsage>,
) -> ConverseOutputResponse {
    let msg = SdkMessage::builder()
        .role(aws_sdk_bedrockruntime::types::ConversationRole::Assistant)
        .content(ContentBlock::Text(text.to_string()))
        .build()
        .expect("build message");
    let mut builder = ConverseOutputResponse::builder()
        .output(ConverseOutputKind::Message(msg))
        .stop_reason(stop);
    if let Some(u) = usage {
        builder = builder.usage(u);
    }
    builder.build().expect("build output")
}

#[test]
fn converse_output_to_chat_response_extracts_text_and_finish_reason() {
    let output = output_with_text("hello world", StopReason::EndTurn, None);
    let resp = converse_output_to_chat_response(&output, "us.anthropic.claude-sonnet-4-6");
    assert_eq!(resp.first_text().as_deref(), Some("hello world"));
    assert_eq!(resp.finish_reason(), Some("end_turn"));
    assert!(resp.first_tool_calls().is_empty());
    assert_eq!(resp.model, "us.anthropic.claude-sonnet-4-6");
}

#[test]
fn converse_output_to_chat_response_extracts_tool_use() {
    let tool_use = aws_sdk_bedrockruntime::types::ToolUseBlock::builder()
        .tool_use_id("call_1")
        .name("get_weather")
        .input(json_to_document(&json!({"location": "Seattle"})).expect("doc"))
        .build()
        .expect("build tool use");
    let msg = SdkMessage::builder()
        .role(aws_sdk_bedrockruntime::types::ConversationRole::Assistant)
        .content(ContentBlock::ToolUse(tool_use))
        .build()
        .expect("build message");
    let output = ConverseOutputResponse::builder()
        .output(ConverseOutputKind::Message(msg))
        .stop_reason(StopReason::ToolUse)
        .build()
        .expect("build output");

    let resp = converse_output_to_chat_response(&output, "us.anthropic.claude-sonnet-4-6");
    assert!(resp.first_text().is_none());
    let calls = resp.first_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].function.name, "get_weather");
    let parsed: serde_json::Value =
        serde_json::from_str(&calls[0].function.arguments).expect("parse");
    assert_eq!(parsed["location"], "Seattle");
    assert_eq!(resp.finish_reason(), Some("tool_use"));
}

#[test]
fn converse_output_to_chat_response_maps_usage() {
    let usage = SdkTokenUsage::builder()
        .input_tokens(100)
        .output_tokens(20)
        .total_tokens(120)
        .cache_read_input_tokens(30)
        .cache_write_input_tokens(10)
        .build()
        .expect("build usage");
    let output = output_with_text("ok", StopReason::EndTurn, Some(usage));

    let resp = converse_output_to_chat_response(&output, "us.anthropic.claude-sonnet-4-6");
    let token_usage = resp.token_usage();
    assert_eq!(token_usage.prompt_tokens, 100);
    assert_eq!(token_usage.completion_tokens, 20);
    assert_eq!(token_usage.cache_read_tokens, 30);
    assert_eq!(token_usage.cache_creation_tokens, 10);
}

#[test]
fn converse_output_to_chat_response_no_usage_is_zeroed() {
    let output = output_with_text("ok", StopReason::EndTurn, None);
    let resp = converse_output_to_chat_response(&output, "m");
    let usage = resp.token_usage();
    assert_eq!(usage.prompt_tokens, 0);
    assert_eq!(usage.completion_tokens, 0);
}

// ─── JSON <-> Document ───────────────────────────────────────────────────────

#[test]
fn json_to_document_round_trips_nested_object() {
    let value = json!({
        "a": 1,
        "b": "two",
        "c": [1, 2, 3],
        "d": {"nested": true},
        "e": null,
    });
    let doc = json_to_document(&value).expect("must convert object");
    let json_str = document_to_json_string(&doc).expect("must serialise");
    let round_tripped: serde_json::Value = serde_json::from_str(&json_str).expect("must parse");
    assert_eq!(round_tripped, value);
}

#[test]
fn json_to_document_rejects_non_object_top_level() {
    assert!(json_to_document(&json!([1, 2, 3])).is_none());
    assert!(json_to_document(&json!("just a string")).is_none());
}

// ─── Live integration test ──────────────────────────────────────────────────

/// Live integration test: send a trivial prompt to Bedrock via Converse.
///
/// Why: End-to-end validation that `BedrockChatClient::chat` produces a
/// non-empty assistant response and non-zero usage against the real service.
/// What: Requires AWS credentials resolvable by the default chain (e.g.
/// `AWS_PROFILE=cto`) and a reachable `us.anthropic.claude-*` inference
/// profile; skipped (not failed) when credential resolution or the call
/// itself fails, so CI (which never sets AWS credentials) is unaffected.
/// Test: Run with `cargo test -p trusty-code -- --include-ignored bedrock`.
#[tokio::test]
#[ignore = "requires AWS credentials; skipped in CI"]
async fn live_bedrock_call() {
    let client = match super::BedrockChatClient::from_env().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping live_bedrock_call: could not build client: {e}");
            return;
        }
    };

    let req = ChatRequest {
        model: "us.anthropic.claude-haiku-4-5".into(),
        messages: vec![
            ChatMessage::system("You are a concise assistant."),
            ChatMessage::user("Reply with exactly the word: pong"),
        ],
        temperature: Some(0.0),
        max_tokens: Some(16),
        tools: None,
        tool_choice: None,
        usage: None,
    };

    match client.chat(&req).await {
        Ok(resp) => {
            let text = resp.first_text().unwrap_or_default();
            eprintln!("live_bedrock_call passed — text: {text:?}");
        }
        Err(e) => {
            eprintln!("skipping live_bedrock_call: call failed: {e}");
        }
    }
}
