//! Unit tests for the Bedrock Converse adapter (#2407, ported from tcode).
//!
//! Why: co-located in a `tests.rs` sibling (via `#[path]`) so `mod.rs`/
//! `convert.rs`/`cache.rs` stay under the 500-SLOC production cap while the
//! conversion logic keeps full coverage.
//! What: region resolution, message/tool-choice/response conversion, the
//! cachePoint translation, and the adapter surface (`name`, `map_tool_choice`,
//! lazy construction) — all offline, no real AWS — plus, since #4426, the
//! `stream_*` suite that drives the REAL `ConverseStream` decode/stream path
//! against a scripted event sequence, and two `#[ignore]`-gated live calls
//! (`Converse` and `ConverseStream`).

use std::collections::VecDeque;

use aws_sdk_bedrockruntime::operation::converse::ConverseOutput as ConverseOutputResponse;
use aws_sdk_bedrockruntime::types::{
    CachePointType, ContentBlock, ContentBlockDelta as SdkContentBlockDelta,
    ContentBlockDeltaEvent, ContentBlockStart as SdkContentBlockStart, ContentBlockStartEvent,
    ContentBlockStopEvent, ConverseOutput as ConverseOutputKind, ConverseStreamMetadataEvent,
    ConverseStreamOutput as ConverseEvent, Message as SdkMessage, MessageStartEvent,
    MessageStopEvent, StopReason, SystemContentBlock, TokenUsage as SdkTokenUsage, Tool as SdkTool,
    ToolChoice as SdkToolChoice, ToolResultStatus, ToolUseBlockDelta, ToolUseBlockStart,
};
use futures_util::StreamExt;
use serde_json::json;

use super::cache::{MIN_CACHEABLE_TOKENS, cache_point_block, system_cacheable, tools_cacheable};
use super::convert::{
    build_converse_messages, build_tool_config, converse_output_to_chat_response,
    document_to_json_string, json_to_document,
};
use super::stream::{ConverseEventSource, drive};
use super::{BedrockAdapter, bedrock_model_id, build_converse_parts, resolve_bedrock_region};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::error::InferenceError;
use crate::inference::streaming::{ChatStreamEvent, StreamAssembly, ToolCallDelta};
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

// ─── cachePoint (#2260) — direct unit tests on `cache` primitives ──────────

/// `cache_point_block` always builds the one shape ever sent: `type: default`,
/// no explicit TTL.
///
/// Why: this is the exact wire shape Bedrock expects for every checkpoint;
/// a wrong `r#type` or a stray TTL would silently change caching behaviour.
/// What: build the block, assert `r#type() == CachePointType::Default` and
/// `ttl().is_none()`.
/// Test: this test.
#[test]
fn cache_point_block_is_default_type() {
    let block = cache_point_block();
    assert_eq!(block.r#type(), &CachePointType::Default);
    assert!(block.ttl().is_none());
}

/// `system_cacheable` respects the `MIN_CACHEABLE_TOKENS` floor: `false` below
/// it, `true` at/above it.
///
/// Why: a checkpoint on a too-small prefix can never produce a cache hit and
/// wastes one of Bedrock's 4-per-request slots; this pins the pure boundary
/// the `build_converse_messages` cachePoint-emission tests rely on.
/// What: assert a short prompt is not cacheable and a prompt well above the
/// floor is.
/// Test: this test.
#[test]
fn system_cacheable_respects_floor() {
    let small = "short prompt";
    let large = "x".repeat(MIN_CACHEABLE_TOKENS * 4 + 100);
    assert!(!system_cacheable(small));
    assert!(system_cacheable(&large));
}

/// `tools_cacheable` respects the same floor, applied to the combined
/// JSON-Schema bodies of a tool set.
///
/// Why: guards `build_tool_config` against wasting a checkpoint on a small
/// tool set (e.g. a single-tool test fixture); this pins the pure boundary.
/// What: assert a tiny single-tool set is not cacheable and a tool set with a
/// large description is.
/// Test: this test.
#[test]
fn tools_cacheable_respects_floor() {
    let tiny_tool = ToolDefinition::function(FunctionDefinition {
        name: "ping".into(),
        description: None,
        parameters: None,
        cache_control: None,
    });
    assert!(!tools_cacheable(&[tiny_tool]));

    let big_tool = ToolDefinition::function(FunctionDefinition {
        name: "write_file".into(),
        description: Some("x".repeat(MIN_CACHEABLE_TOKENS * 4 + 100)),
        parameters: None,
        cache_control: None,
    });
    assert!(tools_cacheable(&[big_tool]));
}

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

// ─── ConverseStream (#4426) ─────────────────────────────────────────────────

/// A scripted [`ConverseEventSource`] standing in for a live `ConverseStream`
/// response.
///
/// Why: the SDK's `EventReceiver` cannot be constructed outside a real HTTP
/// response, so without this double the streaming path could only ever be
/// verified live (i.e. never in CI). Every event TYPE it yields DOES have a
/// public builder, so a scripted sequence of already-decoded events drives the
/// exact production `drive`/`ConverseStreamDecoder` code — only the socket is
/// replaced.
/// What: pops the next scripted `recv` result; an exhausted script reports a
/// clean end of stream (`Ok(None)`), which is what a healthy Bedrock response
/// does after its `Metadata` event.
struct ScriptedEvents {
    script: VecDeque<Result<Option<ConverseEvent>, InferenceError>>,
}

impl ScriptedEvents {
    /// Build a source that yields each event in order, then ends cleanly.
    fn events(events: Vec<ConverseEvent>) -> Self {
        Self {
            script: events.into_iter().map(|e| Ok(Some(e))).collect(),
        }
    }

    /// Build a source that yields each event in order, then FAILS mid-stream.
    fn events_then_error(events: Vec<ConverseEvent>, error: &str) -> Self {
        let mut script: VecDeque<_> = events.into_iter().map(|e| Ok(Some(e))).collect();
        script.push_back(Err(InferenceError::Provider(error.to_string())));
        Self { script }
    }
}

#[async_trait::async_trait]
impl ConverseEventSource for ScriptedEvents {
    async fn recv(&mut self) -> Result<Option<ConverseEvent>, InferenceError> {
        self.script.pop_front().unwrap_or(Ok(None))
    }
}

/// Helper: a `ContentBlockDelta::Text` event on content block 0.
fn text_delta(text: &str) -> ConverseEvent {
    text_delta_at(0, text)
}

/// Helper: a `ContentBlockDelta::Text` event on an explicit content block.
fn text_delta_at(block: i32, text: &str) -> ConverseEvent {
    ConverseEvent::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(SdkContentBlockDelta::Text(text.to_string()))
            .content_block_index(block)
            .build()
            .expect("build ContentBlockDeltaEvent"),
    )
}

/// Helper: the `ContentBlockStart` that introduces a tool call.
fn tool_use_start(block: i32, id: &str, name: &str) -> ConverseEvent {
    ConverseEvent::ContentBlockStart(
        ContentBlockStartEvent::builder()
            .start(SdkContentBlockStart::ToolUse(
                ToolUseBlockStart::builder()
                    .tool_use_id(id)
                    .name(name)
                    .build()
                    .expect("build ToolUseBlockStart"),
            ))
            .content_block_index(block)
            .build()
            .expect("build ContentBlockStartEvent"),
    )
}

/// Helper: a partial-JSON tool-argument fragment for a tool block.
fn tool_use_delta(block: i32, fragment: &str) -> ConverseEvent {
    ConverseEvent::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(SdkContentBlockDelta::ToolUse(
                ToolUseBlockDelta::builder()
                    .input(fragment)
                    .build()
                    .expect("build ToolUseBlockDelta"),
            ))
            .content_block_index(block)
            .build()
            .expect("build ContentBlockDeltaEvent"),
    )
}

/// Helper: the `MessageStop` event carrying a Bedrock stop reason.
fn message_stop(reason: StopReason) -> ConverseEvent {
    ConverseEvent::MessageStop(
        MessageStopEvent::builder()
            .stop_reason(reason)
            .build()
            .expect("build MessageStopEvent"),
    )
}

/// Helper: the terminal `Metadata` event carrying a full token tally.
fn metadata_with_usage(
    input: i32,
    output: i32,
    cache_read: i32,
    cache_write: i32,
) -> ConverseEvent {
    let usage = SdkTokenUsage::builder()
        .input_tokens(input)
        .output_tokens(output)
        .total_tokens(input + output)
        .cache_read_input_tokens(cache_read)
        .cache_write_input_tokens(cache_write)
        .build()
        .expect("build TokenUsage");
    ConverseEvent::Metadata(ConverseStreamMetadataEvent::builder().usage(usage).build())
}

/// Helper: collect a whole [`ChatStream`] into a vector.
async fn collect_stream(
    mut stream: crate::inference::streaming::ChatStream,
) -> Vec<Result<ChatStreamEvent, InferenceError>> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

/// Text deltas reach the consumer in arrival order, followed by exactly one
/// terminal `Done`.
///
/// Why (#4426): out-of-order or dropped concatenation is the most visible
/// streaming defect — the user reads scrambled prose — and a missing terminal
/// event strands any consumer waiting for usage.
/// What: script two text deltas plus a clean end; assert `Delta`, `Delta`,
/// `Done` and nothing else.
/// Test: this test.
#[tokio::test]
async fn stream_forwards_text_deltas_in_order() {
    let events = collect_stream(drive(ScriptedEvents::events(vec![
        text_delta("He"),
        text_delta("llo"),
    ])))
    .await;

    assert_eq!(events.len(), 3, "expected two deltas + Done: {events:?}");
    assert_eq!(
        events[0].as_ref().expect("delta"),
        &ChatStreamEvent::Delta("He".to_string())
    );
    assert_eq!(
        events[1].as_ref().expect("delta"),
        &ChatStreamEvent::Delta("llo".to_string())
    );
    assert!(matches!(
        events[2].as_ref().expect("done"),
        ChatStreamEvent::Done(_)
    ));
}

/// Structural framing events forward nothing.
///
/// Why: `MessageStart`/`ContentBlockStop` (and any future SDK `Unknown`
/// variant) carry no assistant content; emitting anything for them would inject
/// phantom deltas into the rendered turn.
/// What: script a `MessageStart` and a `ContentBlockStop` around one real
/// delta; assert only that delta plus `Done` come out. An EMPTY text delta is
/// included too — Bedrock can frame one, and it must not surface as a token.
/// Test: this test.
#[tokio::test]
async fn stream_ignores_structural_events() {
    let events = collect_stream(drive(ScriptedEvents::events(vec![
        ConverseEvent::MessageStart(
            MessageStartEvent::builder()
                .role(aws_sdk_bedrockruntime::types::ConversationRole::Assistant)
                .build()
                .expect("build MessageStartEvent"),
        ),
        text_delta(""),
        text_delta("hi"),
        ConverseEvent::ContentBlockStop(
            ContentBlockStopEvent::builder()
                .content_block_index(0)
                .build()
                .expect("build ContentBlockStopEvent"),
        ),
    ])))
    .await;

    assert_eq!(
        events.len(),
        2,
        "only the non-empty delta + Done: {events:?}"
    );
    assert_eq!(
        events[0].as_ref().expect("delta"),
        &ChatStreamEvent::Delta("hi".to_string())
    );
    assert!(matches!(
        events[1].as_ref().expect("done"),
        ChatStreamEvent::Done(_)
    ));
}

/// `MessageStop`'s stop reason and `Metadata`'s token tally both land in the
/// single terminal `Done`.
///
/// Why (#4426): Bedrock splits the terminal summary across two events, neither
/// of which is last, while the neutral contract promises ONE `Done` carrying
/// both. Dropping the `Metadata` tally would silently zero every streamed
/// Bedrock turn for cost/telemetry consumers — the same defect #3767 fixed on
/// the `chat::bedrock_impl` path.
/// What: script a delta, a `MessageStop(EndTurn)`, and a `Metadata` with all
/// four token buckets; assert the terminal event carries the lowercased stop
/// reason and every bucket.
/// Test: this test.
#[tokio::test]
async fn stream_carries_usage_and_stop_reason_in_terminal() {
    let events = collect_stream(drive(ScriptedEvents::events(vec![
        text_delta("ok"),
        message_stop(StopReason::EndTurn),
        metadata_with_usage(100, 20, 30, 10),
    ])))
    .await;

    let ChatStreamEvent::Done(completion) = events.last().expect("terminal").as_ref().expect("ok")
    else {
        panic!("last event must be Done: {events:?}");
    };
    assert_eq!(
        completion.finish_reason,
        Some(crate::inference::types::StopReason::Other(
            "end_turn".into()
        )),
        "Bedrock's stopReason must be lowercased, exactly as the buffered path does"
    );
    assert_eq!(completion.usage.prompt_tokens, 100);
    assert_eq!(completion.usage.completion_tokens, 20);
    assert_eq!(completion.usage.cache_read_tokens, 30);
    assert_eq!(completion.usage.cache_creation_tokens, 10);
}

/// A `Metadata` event with no `usage` must not fabricate a zeroed tally over a
/// real one.
///
/// Why: Bedrock omits `usage` on some guardrail/error paths; treating an absent
/// tally as "all zeros" is fine only because the decoder starts zeroed — the
/// bug to guard against is a later empty `Metadata` ERASING a tally already
/// reported.
/// What: script a usage-bearing `Metadata` followed by an empty one; assert the
/// terminal event still carries the real numbers.
/// Test: this test.
#[tokio::test]
async fn stream_metadata_without_usage_does_not_clobber() {
    let events = collect_stream(drive(ScriptedEvents::events(vec![
        metadata_with_usage(7, 3, 0, 0),
        ConverseEvent::Metadata(ConverseStreamMetadataEvent::builder().build()),
    ])))
    .await;

    let ChatStreamEvent::Done(completion) = events.last().expect("terminal").as_ref().expect("ok")
    else {
        panic!("last event must be Done: {events:?}");
    };
    assert_eq!(completion.usage.prompt_tokens, 7);
    assert_eq!(completion.usage.completion_tokens, 3);
}

/// A streamed tool call maps to an introducing `ToolCall` (id + name) followed
/// by argument-fragment `ToolCall`s.
///
/// Why (#4426): the reference `chat::bedrock_impl` path IGNORES tool use
/// entirely — porting that gap would have left the shared adapter unable to
/// stream an agent turn, which is trusty-code's whole use case. Converse
/// introduces the call in `ContentBlockStart` and streams its arguments as
/// partial JSON in later deltas, so both must be forwarded.
/// What: script a tool-use start plus two argument fragments; assert the
/// introducing event carries id/name with empty arguments and the continuations
/// carry only the fragments, all on the same slot index.
/// Test: this test.
#[tokio::test]
async fn stream_maps_tool_use_fragments() {
    let events = collect_stream(drive(ScriptedEvents::events(vec![
        tool_use_start(0, "call_1", "get_weather"),
        tool_use_delta(0, "{\"location\":"),
        tool_use_delta(0, "\"Seattle\"}"),
        message_stop(StopReason::ToolUse),
    ])))
    .await;

    assert_eq!(events.len(), 4, "3 tool events + Done: {events:?}");
    assert_eq!(
        events[0].as_ref().expect("start"),
        &ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("get_weather".into()),
            arguments: String::new(),
        })
    );
    assert_eq!(
        events[1].as_ref().expect("fragment"),
        &ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: "{\"location\":".into(),
        })
    );
    assert_eq!(
        events[2].as_ref().expect("fragment"),
        &ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: "\"Seattle\"}".into(),
        })
    );
}

/// Tool-call slots are dense from zero even when text precedes the tool block.
///
/// Why: Converse's `contentBlockIndex` counts EVERY content block, so a turn
/// that says something before calling a tool puts the tool on block 1. Passing
/// that raw index through as the tool-call ordinal would number the turn's only
/// call `1` — an OpenAI-dialect consumer indexing calls densely from 0 would
/// then see a phantom empty call at slot 0.
/// What: script a text block (index 0) then two tool blocks (indices 1 and 2);
/// assert the tool slots are 0 and 1.
/// Test: this test.
#[tokio::test]
async fn stream_tool_slots_are_dense_from_zero() {
    let events = collect_stream(drive(ScriptedEvents::events(vec![
        text_delta_at(0, "let me check"),
        tool_use_start(1, "call_a", "alpha"),
        tool_use_start(2, "call_b", "beta"),
        tool_use_delta(1, "{}"),
    ])))
    .await;

    let slots: Vec<(usize, Option<String>)> = events
        .iter()
        .filter_map(|e| match e.as_ref().expect("ok") {
            ChatStreamEvent::ToolCall(d) => Some((d.index, d.id.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        slots,
        vec![
            (0, Some("call_a".to_string())),
            (1, Some("call_b".to_string())),
            (0, None),
        ],
        "tool slots must be dense from 0 and stable per content block"
    );
}

/// A mid-stream failure ends the stream with `Err` and NEVER a `Done`.
///
/// Why: a truncated stream that terminates with a clean `Done` is
/// indistinguishable from a complete short answer — the caller would record a
/// partial turn as successful. This is the same dual-channel failure contract
/// the SSE lane (`decode_event_stream`) and `chat::bedrock_impl` both hold.
/// What: script one delta then a provider error; assert the delta arrives, the
/// next item is `Err`, and the stream ends there.
/// Test: this test.
#[tokio::test]
async fn stream_surfaces_mid_stream_error() {
    let events = collect_stream(drive(ScriptedEvents::events_then_error(
        vec![text_delta("partial ")],
        "ConverseStream failed mid-stream: throttled",
    )))
    .await;

    assert_eq!(events.len(), 2, "delta + terminal Err: {events:?}");
    assert_eq!(
        events[0].as_ref().expect("delta"),
        &ChatStreamEvent::Delta("partial ".to_string())
    );
    let err = events[1].as_ref().expect_err("must be an error");
    assert!(
        err.to_string().contains("throttled"),
        "error must carry the provider reason: {err}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Ok(ChatStreamEvent::Done(_)))),
        "a failed stream must not also emit Done: {events:?}"
    );
}

/// A streamed turn rebuilds into the SAME `ChatResponse` the buffered
/// `Converse` path returns for the equivalent output.
///
/// Why (#4426): this is the property that makes streaming a transport detail.
/// If the streamed finish reason, text, or usage differed from the buffered
/// one, every downstream consumer (transcript recording, cost accrual, the tool
/// loop) would need to branch on which transport ran. Asserting against the
/// REAL `converse_output_to_chat_response` output — not a hand-written expected
/// value — means a future change to either side that breaks the equivalence
/// fails here.
/// What: stream `"hello world"` with `EndTurn` and a token tally, assemble via
/// [`StreamAssembly`], and compare against the buffered conversion of a
/// `ConverseOutput` carrying the same content.
/// Test: this test.
#[tokio::test]
async fn stream_assembles_into_buffered_shaped_response() {
    let usage = SdkTokenUsage::builder()
        .input_tokens(100)
        .output_tokens(20)
        .total_tokens(120)
        .cache_read_input_tokens(30)
        .cache_write_input_tokens(10)
        .build()
        .expect("build usage");
    let buffered = converse_output_to_chat_response(
        &output_with_text("hello world", StopReason::EndTurn, Some(usage)),
        "bedrock/us.anthropic.claude-sonnet-4-6",
    );

    let mut assembly = StreamAssembly::new();
    let mut stream = drive(ScriptedEvents::events(vec![
        text_delta("hello"),
        text_delta(" world"),
        message_stop(StopReason::EndTurn),
        metadata_with_usage(100, 20, 30, 10),
    ]));
    while let Some(item) = stream.next().await {
        assembly.push(item.expect("no stream error"));
    }
    let streamed = assembly.into_response(
        buffered.id.clone(),
        "bedrock/us.anthropic.claude-sonnet-4-6",
    );

    // `ChatResponse` has no `PartialEq` (it is a wire type, not a value type),
    // so compare the serialised forms — which is a STRICTER check than a
    // field-by-field assertion: a new field added to either path shows up here.
    assert_eq!(
        serde_json::to_value(&streamed).expect("serialise streamed"),
        serde_json::to_value(&buffered).expect("serialise buffered"),
        "a streamed turn must rebuild into exactly the buffered response"
    );
}

/// Both Converse transports build their request from ONE conversion.
///
/// Why (#4426): `Converse` and `ConverseStream` have different fluent builders,
/// so the shared conversion is the only thing preventing the two paths from
/// drifting (a sampling knob, a cachePoint, or a tool config honoured on one
/// and not the other). This pins the shared builder directly, so a regression
/// in it fails a test rather than only showing up as a live behaviour
/// difference.
/// What: build parts from a request carrying a system prompt, sampling knobs,
/// and a tool; assert every piece is populated.
/// Test: this test.
#[test]
fn build_converse_parts_carries_system_sampling_and_tools() {
    let req = ChatRequest {
        model: "bedrock/us.anthropic.claude-sonnet-4-6".into(),
        messages: vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("what is the weather?"),
        ],
        temperature: Some(0.25),
        max_tokens: Some(512),
        tools: Some(vec![sample_tool()]),
        tool_choice: Some(json!({"auto": {}})),
        stop: None,
        usage: None,
    };

    let parts = build_converse_parts(&req).expect("parts must build");
    assert_eq!(parts.system.len(), 1, "system prompt must be diverted");
    assert_eq!(parts.messages.len(), 1, "one user message");
    assert_eq!(parts.inference.max_tokens(), Some(512));
    assert_eq!(parts.inference.temperature(), Some(0.25));
    let tool_config = parts.tool_config.expect("tool config must be built");
    assert_eq!(tool_config.tools().len(), 1);
}

/// A request with no tools omits `toolConfig` entirely.
///
/// Why: Converse rejects an empty `toolConfig`, so "no tools" must mean the
/// field is absent, not present-and-empty.
/// What: build parts from a tool-free request; assert `tool_config` is `None`.
/// Test: this test.
#[test]
fn build_converse_parts_omits_tool_config_without_tools() {
    let req = ChatRequest {
        model: "bedrock/us.anthropic.claude-sonnet-4-6".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stop: None,
        usage: None,
    };
    let parts = build_converse_parts(&req).expect("parts must build");
    assert!(parts.tool_config.is_none());
    assert!(parts.system.is_empty());
    assert_eq!(parts.inference.max_tokens(), None);
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

/// Live integration test: stream a trivial prompt from Bedrock via
/// `ConverseStream` (#4426).
///
/// Why: the offline `stream_*` tests inject already-decoded events, so they
/// cannot exercise credential resolution, the `ConverseStream` handshake, or
/// the SDK's binary event-stream wire decoding — the three things that can only
/// fail against the real service. This is the end-to-end proof that
/// [`BedrockAdapter::chat_stream`] streams; unlike the offline suite it FAILS
/// (rather than skips) on a mid-stream error, since a caller who opts into this
/// test has credentials and wants a real verdict.
/// What: requires AWS credentials resolvable by the default chain (e.g.
/// `AWS_PROFILE=cto`) and a reachable `us.anthropic.claude-*` inference profile.
/// Asserts MORE THAN ONE event carrying text arrived (one delta would mean the
/// buffered fallback was still in play), that the concatenated text is
/// non-empty, and that the terminal `Done` reports non-zero usage.
/// Test: run with `cargo test -p trusty-common --features bedrock-client -- \
///        --include-ignored live_bedrock_converse_stream --nocapture`.
#[tokio::test]
#[ignore = "requires AWS credentials; skipped in CI"]
async fn live_bedrock_converse_stream() {
    let adapter = BedrockAdapter::new(None);
    let req = ChatRequest {
        model: "bedrock/us.anthropic.claude-sonnet-4-6".into(),
        messages: vec![
            ChatMessage::system("You are a concise assistant."),
            ChatMessage::user("Count from one to ten in words, separated by commas."),
        ],
        temperature: Some(0.0),
        max_tokens: Some(128),
        tools: None,
        tool_choice: None,
        stop: None,
        usage: None,
    };

    let mut stream = adapter
        .chat_stream(&req)
        .await
        .expect("ConverseStream handshake must succeed");

    let mut deltas = 0usize;
    let mut text = String::new();
    let mut completion = None;
    while let Some(item) = stream.next().await {
        match item.expect("no mid-stream error") {
            ChatStreamEvent::Delta(chunk) => {
                deltas += 1;
                eprintln!("delta {deltas}: {chunk:?}");
                text.push_str(&chunk);
            }
            ChatStreamEvent::ToolCall(call) => eprintln!("tool call: {call:?}"),
            ChatStreamEvent::Done(done) => completion = Some(done),
        }
    }

    let done = completion.expect("stream must end with a terminal Done");
    eprintln!("live_bedrock_converse_stream — {deltas} deltas, done: {done:?}");
    assert!(!text.trim().is_empty(), "streamed text must be non-empty");
    assert!(
        deltas > 1,
        "a real ConverseStream turn arrives in MANY deltas; {deltas} would mean \
         the buffered fallback is still in play"
    );
    assert!(
        done.usage.prompt_tokens > 0 && done.usage.completion_tokens > 0,
        "the terminal Metadata tally must survive: {:?}",
        done.usage
    );
}
