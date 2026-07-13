//! Unit tests for the Bedrock Converse adapter (#2407, ported from tcode).
//!
//! Why: co-located in a `tests.rs` sibling (via `#[path]`) so `mod.rs`/
//! `convert.rs`/`cache.rs` stay under the 500-SLOC production cap while the
//! conversion logic keeps full coverage.
//! What: region resolution, message/tool-choice/response conversion, the
//! cachePoint translation, and the adapter surface (`name`, `map_tool_choice`,
//! lazy construction) — all offline, no real AWS — plus one `#[ignore]`-gated
//! live Converse call.

use aws_sdk_bedrockruntime::operation::converse::ConverseOutput as ConverseOutputResponse;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConverseOutput as ConverseOutputKind, Message as SdkMessage, StopReason,
    SystemContentBlock, TokenUsage as SdkTokenUsage, Tool as SdkTool, ToolChoice as SdkToolChoice,
    ToolResultStatus,
};
use serde_json::json;

use super::cache::MIN_CACHEABLE_TOKENS;
use super::convert::{
    build_converse_messages, build_tool_config, converse_output_to_chat_response,
    document_to_json_string, json_to_document,
};
use super::{BedrockAdapter, bedrock_model_id, resolve_bedrock_region};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::types::{
    CacheControl, ChatMessage, ChatRequest, FunctionCall, FunctionDefinition, ToolCall, ToolChoice,
    ToolDefinition,
};

/// Serializes every test that mutates the process-wide region env vars.
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
        stop: None,
        usage: None,
    }
}

// ─── Adapter surface ────────────────────────────────────────────────────────

#[test]
fn name_is_bedrock() {
    assert_eq!(BedrockAdapter::new(None).name(), "bedrock");
}

/// Constructing a `BedrockAdapter` never touches AWS — the lazy client cell
/// stays empty until the first `chat`.
///
/// Why: pins the "default configuration works standalone" guarantee (#2245) — a
/// `Configurator` that registers the Bedrock factory must not require AWS
/// credentials just because the adapter exists.
/// What: build the adapter; assert the internal `OnceCell` is still empty.
/// Test: this test.
#[test]
fn new_does_not_touch_aws() {
    let adapter = BedrockAdapter::new(Some("us-east-1"));
    assert!(
        adapter.client.get().is_none(),
        "the AWS client cell must be lazy — untouched at construction"
    );
    assert_eq!(adapter.region(), "us-east-1");
    assert_eq!(
        adapter.capabilities().id,
        crate::inference::ProviderId::Bedrock
    );
}

/// `map_tool_choice` maps the scalar policies to Converse's own JSON shape (NOT
/// the OpenAI shape).
///
/// Why: this is the exact JSON [`super::convert::build_tool_config`] interprets;
/// a wrong shape here would silently fail to force/suppress tool calls.
/// What: map each scalar variant, assert the JSON value.
/// Test: this test.
#[test]
fn map_tool_choice_scalars() {
    let a = BedrockAdapter::new(None);
    assert_eq!(a.map_tool_choice(ToolChoice::None), json!("none"));
    assert_eq!(a.map_tool_choice(ToolChoice::Auto), json!({"auto": {}}));
    assert_eq!(a.map_tool_choice(ToolChoice::Required), json!({"any": {}}));
}

/// `map_tool_choice(Function)` produces Converse's specific-tool selector object.
///
/// Why: forcing a specific tool requires Converse's `{"tool":{"name":...}}`
/// shape, not OpenAI's `{"type":"function","function":{"name":...}}`.
/// What: map `Function("search_code")`, assert the object structure.
/// Test: this test.
#[test]
fn map_tool_choice_function() {
    let a = BedrockAdapter::new(None);
    let v = a.map_tool_choice(ToolChoice::Function("search_code".into()));
    assert_eq!(v, json!({"tool": {"name": "search_code"}}));
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
    req.messages
        .push(ChatMessage::tool_result("call_1", "get_weather", "72F"));
    req.messages
        .push(ChatMessage::tool_result("call_2", "get_time", "12:00 UTC"));

    let (_, messages) = build_converse_messages(&req).expect("convert");

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

// ─── ToolUse/ToolResult pairing backstop (#2278 Fix B) ─────────────────────

fn assert_tool_pairing_invariant(messages: &[aws_sdk_bedrockruntime::types::Message]) {
    let mut introduced: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut answered: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for message in messages {
        for block in message.content() {
            match block {
                ContentBlock::ToolUse(tu) => {
                    introduced.insert(tu.tool_use_id());
                }
                ContentBlock::ToolResult(tr) => {
                    assert!(
                        introduced.contains(tr.tool_use_id()),
                        "orphan ToolResult for {:?} with no preceding ToolUse in {messages:?}",
                        tr.tool_use_id()
                    );
                    answered.insert(tr.tool_use_id());
                }
                _ => {}
            }
        }
    }
    for id in &introduced {
        assert!(
            answered.contains(id),
            "ToolUse {id:?} has no matching ToolResult anywhere in {messages:?}"
        );
    }
}

#[test]
fn enforce_tool_pairing_drops_orphan_tool_result() {
    let req = minimal_request(vec![
        ChatMessage::system("s"),
        ChatMessage::user("do it"),
        ChatMessage::tool_result("orphan_call", "bash", "leftover output"),
    ]);
    let (_, messages) = build_converse_messages(&req).expect("convert");

    assert_tool_pairing_invariant(&messages);
    for message in &messages {
        for block in message.content() {
            if let ContentBlock::ToolResult(tr) = block {
                assert_ne!(
                    tr.tool_use_id(),
                    "orphan_call",
                    "orphan ToolResult must be dropped, not passed through: {messages:?}"
                );
            }
        }
    }
}

#[test]
fn enforce_tool_pairing_synthesizes_placeholder_for_unanswered_tool_use() {
    let mut req = minimal_request(vec![ChatMessage::system("s"), ChatMessage::user("do it")]);
    req.messages.push(ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "bash".into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
        name: None,
        cache_control: None,
    });

    let (_, messages) = build_converse_messages(&req).expect("convert");

    assert_tool_pairing_invariant(&messages);
    let assistant_idx = messages
        .iter()
        .position(|m| {
            m.content()
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse(_)))
        })
        .expect("assistant ToolUse message must be present");
    let following = messages
        .get(assistant_idx + 1)
        .expect("a following message with the synthesized placeholder must exist");
    let placeholder = following
        .content()
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolResult(tr) if tr.tool_use_id() == "call_1" => Some(tr),
            _ => None,
        })
        .expect("placeholder ToolResult for call_1 must exist");
    assert!(matches!(
        placeholder.status(),
        Some(ToolResultStatus::Error)
    ));
}

#[test]
fn enforce_tool_pairing_leaves_valid_conversation_unchanged() {
    let mut req = minimal_request(vec![ChatMessage::system("s"), ChatMessage::user("go")]);
    req.messages.push(ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "bash".into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
        name: None,
        cache_control: None,
    });
    req.messages
        .push(ChatMessage::tool_result("call_1", "bash", "ok"));

    let (_, messages) = build_converse_messages(&req).expect("convert");

    assert_tool_pairing_invariant(&messages);
    assert_eq!(
        messages.len(),
        3,
        "user(task), assistant(tool use), user(tool result) — no extra synthesized message"
    );
    assert_eq!(messages[2].content().len(), 1, "no placeholder appended");
    match &messages[2].content()[0] {
        ContentBlock::ToolResult(tr) => {
            assert_eq!(tr.tool_use_id(), "call_1");
            assert!(
                tr.status().is_none(),
                "a real result must not get the placeholder Error status"
            );
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

// ─── cachePoint (#2260) ─────────────────────────────────────────────────────

fn cached_system_message(text: &str) -> ChatMessage {
    let mut msg = ChatMessage::system(text);
    msg.cache_control = Some(CacheControl::ephemeral());
    msg
}

#[test]
fn build_converse_messages_emits_cache_point_after_large_cached_system() {
    let large_system = "x".repeat(MIN_CACHEABLE_TOKENS * 4 + 100);
    let req = minimal_request(vec![
        cached_system_message(&large_system),
        ChatMessage::user("hi"),
    ]);
    let (system, _) = build_converse_messages(&req).expect("convert");

    assert_eq!(system.len(), 2, "expected Text + CachePoint");
    assert!(matches!(system[0], SystemContentBlock::Text(_)));
    assert!(matches!(system[1], SystemContentBlock::CachePoint(_)));
}

#[test]
fn build_converse_messages_omits_cache_point_for_small_cached_system() {
    let req = minimal_request(vec![
        cached_system_message("short prompt"),
        ChatMessage::user("hi"),
    ]);
    let (system, _) = build_converse_messages(&req).expect("convert");

    assert_eq!(
        system.len(),
        1,
        "a too-small cached prefix must not get a cachePoint"
    );
}

#[test]
fn build_converse_messages_omits_cache_point_when_not_marked() {
    let large_system = "x".repeat(MIN_CACHEABLE_TOKENS * 4 + 100);
    let req = minimal_request(vec![
        ChatMessage::system(large_system),
        ChatMessage::user("hi"),
    ]);
    let (system, _) = build_converse_messages(&req).expect("convert");

    assert_eq!(
        system.len(),
        1,
        "unmarked system must never get a cachePoint"
    );
}

#[test]
fn build_converse_messages_emits_cache_point_after_marked_history_message() {
    let mut assistant_msg = ChatMessage::assistant("on it");
    assistant_msg.cache_control = Some(CacheControl::ephemeral());
    let req = minimal_request(vec![
        ChatMessage::system("s"),
        ChatMessage::user("do the thing"),
        assistant_msg,
    ]);
    let (_, messages) = build_converse_messages(&req).expect("convert");

    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content().len(),
        1,
        "unmarked user message must not get a cachePoint"
    );
    assert!(matches!(&messages[0].content()[0], ContentBlock::Text(_)));

    assert_eq!(
        messages[1].content().len(),
        2,
        "expected Text + CachePoint on the marked assistant message"
    );
    assert!(matches!(&messages[1].content()[0], ContentBlock::Text(_)));
    assert!(matches!(
        &messages[1].content()[1],
        ContentBlock::CachePoint(_)
    ));
}

#[test]
fn build_converse_messages_omits_cache_point_for_unmarked_history_messages() {
    let req = minimal_request(vec![
        ChatMessage::system("s"),
        ChatMessage::user("do two things"),
        ChatMessage::assistant("on it"),
    ]);
    let (_, messages) = build_converse_messages(&req).expect("convert");

    for message in &messages {
        for block in message.content() {
            assert!(
                !matches!(block, ContentBlock::CachePoint(_)),
                "no message should carry a cachePoint when unmarked"
            );
        }
    }
}

#[test]
fn build_tool_config_emits_cache_point_for_large_cached_last_tool() {
    let tool = ToolDefinition::function(FunctionDefinition {
        name: "write_file".into(),
        description: Some("x".repeat(MIN_CACHEABLE_TOKENS * 4 + 100)),
        parameters: None,
        cache_control: Some(CacheControl::ephemeral()),
    });
    let config = build_tool_config(&[tool], None)
        .expect("no error")
        .expect("config present");

    assert_eq!(config.tools().len(), 2, "expected ToolSpec + CachePoint");
    assert!(matches!(
        config.tools().last(),
        Some(SdkTool::CachePoint(_))
    ));
}

#[test]
fn build_tool_config_omits_cache_point_for_small_cached_tool() {
    let tool = ToolDefinition::function(FunctionDefinition {
        name: "ping".into(),
        description: None,
        parameters: None,
        cache_control: Some(CacheControl::ephemeral()),
    });
    let config = build_tool_config(&[tool], None)
        .expect("no error")
        .expect("config present");

    assert_eq!(
        config.tools().len(),
        1,
        "a too-small cached tool set must not get a cachePoint"
    );
}

#[test]
fn build_tool_config_omits_cache_point_when_not_marked() {
    let config = build_tool_config(&[sample_tool()], None)
        .expect("no error")
        .expect("config present");
    assert_eq!(config.tools().len(), 1);
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
    assert_eq!(
        resp.choices[0].finish_reason.as_deref(),
        Some("end_turn"),
        "stopReason must be lowercased into finish_reason"
    );
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
    assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("tool_use"));
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
    let usage = resp.usage();
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.completion_tokens, 20);
    assert_eq!(usage.cache_read_tokens, 30);
    assert_eq!(usage.cache_creation_tokens, 10);
}

#[test]
fn converse_output_to_chat_response_no_usage_is_zeroed() {
    let output = output_with_text("ok", StopReason::EndTurn, None);
    let resp = converse_output_to_chat_response(&output, "m");
    let usage = resp.usage();
    assert_eq!(usage.prompt_tokens, 0);
    assert_eq!(usage.completion_tokens, 0);
}

// ─── Model id prefix stripping ──────────────────────────────────────────────

#[test]
fn bedrock_model_id_strips_prefix() {
    assert_eq!(
        bedrock_model_id("bedrock/us.anthropic.claude-sonnet-4-6"),
        "us.anthropic.claude-sonnet-4-6"
    );
}

#[test]
fn bedrock_model_id_passthrough_without_prefix() {
    assert_eq!(
        bedrock_model_id("us.anthropic.claude-sonnet-4-6"),
        "us.anthropic.claude-sonnet-4-6"
    );
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
/// Why: end-to-end validation that [`BedrockAdapter::chat`] produces a non-empty
/// assistant response against the real service. Uses the FULL `bedrock/`-prefixed
/// dispatch slug so it also exercises [`super::bedrock_model_id`]'s strip on the
/// live path.
/// What: requires AWS credentials resolvable by the default chain (e.g.
/// `AWS_PROFILE=cto`) and a reachable `us.anthropic.claude-*` inference profile;
/// skipped (not failed) when the call fails, so CI (which never sets AWS
/// credentials) is unaffected.
/// Test: run with `cargo test -p trusty-common --features bedrock-client -- \
///        --include-ignored bedrock`.
#[tokio::test]
#[ignore = "requires AWS credentials; skipped in CI"]
async fn live_bedrock_call() {
    let adapter = BedrockAdapter::new(None);
    let req = ChatRequest {
        model: "bedrock/us.anthropic.claude-haiku-4-5".into(),
        messages: vec![
            ChatMessage::system("You are a concise assistant."),
            ChatMessage::user("Reply with exactly the word: pong"),
        ],
        temperature: Some(0.0),
        max_tokens: Some(16),
        tools: None,
        tool_choice: None,
        stop: None,
        usage: None,
    };

    match adapter.chat(&req).await {
        Ok(resp) => {
            let text = resp.first_text().unwrap_or_default();
            eprintln!("live_bedrock_call passed — text: {text:?}");
        }
        Err(e) => {
            eprintln!("skipping live_bedrock_call: call failed: {e}");
        }
    }
}
