//! Unit tests for the `inference_bridge` conversion seam.
//!
//! Why: kept in its own file so `mod.rs` stays under the 500-SLOC production
//! cap while every conversion path (and the two documented compromises) gets
//! explicit coverage.
//! What: message round-trips per role, tool-call/tool-definition mapping,
//! `ToolChoice` mapping, response mapping, and the multi-part-content /
//! cache-control compromise probes called out in the module doc.
//! Test: this IS the test module.

use super::*;
use async_openai::types::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageArgs, ChatCompletionToolArgs, FunctionObjectArgs, ImageUrl,
};

fn sys(text: &str) -> ChatCompletionRequestMessage {
    ChatCompletionRequestSystemMessageArgs::default()
        .content(text)
        .build()
        .unwrap()
        .into()
}

fn user(text: &str) -> ChatCompletionRequestMessage {
    ChatCompletionRequestUserMessageArgs::default()
        .content(text)
        .build()
        .unwrap()
        .into()
}

fn tool_msg(call_id: &str, content: &str) -> ChatCompletionRequestMessage {
    ChatCompletionRequestToolMessageArgs::default()
        .tool_call_id(call_id)
        .content(content)
        .build()
        .unwrap()
        .into()
}

fn make_tool(name: &str, params: serde_json::Value) -> ChatCompletionTool {
    let func = FunctionObjectArgs::default()
        .name(name)
        .description("a tool")
        .parameters(params)
        .build()
        .unwrap();
    ChatCompletionToolArgs::default()
        .function(func)
        .build()
        .unwrap()
}

/// Why: role + text + `name` must survive the system-message conversion.
/// Test: itself.
#[test]
fn system_message_maps_role_and_text() {
    let shared = to_shared_message(&sys("you are helpful"));
    assert_eq!(shared.role, "system");
    assert_eq!(shared.content.as_deref(), Some("you are helpful"));
    assert!(shared.cache_control.is_none());
}

/// Why: the user-message path is the most common turn; role/content/name must
/// carry through unchanged.
/// Test: itself.
#[test]
fn user_message_maps_role_and_text() {
    let shared = to_shared_message(&user("hello there"));
    assert_eq!(shared.role, "user");
    assert_eq!(shared.content.as_deref(), Some("hello there"));
}

/// Why: assistant turns with tool calls must carry the calls AND leave
/// `content` as whatever async-openai reports (`None` for tool-only turns).
/// Test: itself.
#[test]
fn assistant_message_with_tool_calls_maps_fields() {
    let call = ChatCompletionMessageToolCall {
        id: "call_1".into(),
        r#type: ChatCompletionToolType::Function,
        function: FunctionCall {
            name: "get_weather".into(),
            arguments: r#"{"loc":"SEA"}"#.into(),
        },
    };
    let msg: ChatCompletionRequestMessage = ChatCompletionRequestAssistantMessageArgs::default()
        .tool_calls(vec![call])
        .build()
        .unwrap()
        .into();
    let shared = to_shared_message(&msg);
    assert_eq!(shared.role, "assistant");
    assert!(shared.content.is_none());
    let calls = shared.tool_calls.expect("tool_calls present");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].kind, "function");
    assert_eq!(calls[0].function.name, "get_weather");
    assert_eq!(calls[0].function.arguments, r#"{"loc":"SEA"}"#);
}

/// Why: tool-result messages must carry `tool_call_id` — the field the next
/// turn's pairing depends on.
/// Test: itself.
#[test]
fn tool_message_maps_fields() {
    let shared = to_shared_message(&tool_msg("call_abc", r#"{"t":72}"#));
    assert_eq!(shared.role, "tool");
    assert_eq!(shared.tool_call_id.as_deref(), Some("call_abc"));
    assert_eq!(shared.content.as_deref(), Some(r#"{"t":72}"#));
}

/// Documents compromise #2 (module doc): a multi-part user content array
/// collapses to the concatenation of its text parts, and non-text parts
/// (image/audio) are dropped rather than erroring.
///
/// Why: `async-openai`'s content model technically allows this shape even
/// though trusty-agents never constructs it (verified — see module doc); the
/// bridge must degrade predictably rather than panic if it ever does.
/// Test: itself.
#[test]
fn multi_part_user_content_collapses_to_concatenated_text() {
    use async_openai::types::{
        ChatCompletionRequestMessageContentPartImage, ChatCompletionRequestMessageContentPartText,
        ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
    };
    let msg = ChatCompletionRequestUserMessage {
        content: ChatCompletionRequestUserMessageContent::Array(vec![
            ChatCompletionRequestUserMessageContentPart::Text(
                ChatCompletionRequestMessageContentPartText {
                    text: "look at this: ".into(),
                },
            ),
            ChatCompletionRequestUserMessageContentPart::ImageUrl(
                ChatCompletionRequestMessageContentPartImage {
                    image_url: ImageUrl {
                        url: "https://example.test/x.png".into(),
                        detail: None,
                    },
                },
            ),
            ChatCompletionRequestUserMessageContentPart::Text(
                ChatCompletionRequestMessageContentPartText {
                    text: "what is it?".into(),
                },
            ),
        ]),
        name: None,
    };
    let shared = to_shared_message(&ChatCompletionRequestMessage::User(msg));
    assert_eq!(
        shared.content.as_deref(),
        Some("look at this: what is it?"),
        "text parts concatenate; the image part is dropped, not errored"
    );
}

/// The shared wire type's own cache_control→content-block serialisation is
/// intact and ready to receive a marker from a future producer, even though
/// THIS bridge can never set one (module doc compromise #1 — async-openai's
/// `ChatCompletionRequestMessage` has no `cache_control` field to read from).
///
/// Why: proves the seam itself is not broken, only that this particular
/// source type cannot feed it — the distinction the PR report calls out.
/// Test: itself.
#[test]
fn shared_chat_message_cache_control_still_serialises_as_block() {
    let mut m = tci::ChatMessage::system("prefix");
    assert!(m.cache_control.is_none(), "bridge never sets this field");
    m.cache_control = Some(tci::CacheControl::ephemeral());
    let v = serde_json::to_value(&m).expect("serialise");
    assert_eq!(
        v["content"],
        serde_json::json!([{
            "type": "text",
            "text": "prefix",
            "cache_control": {"type": "ephemeral"}
        }])
    );
}

/// Why: `ToolRegistry::openai_tools()`'s output must map onto the shared
/// tool-schema type without dropping name/description/parameters.
/// Test: itself.
#[test]
fn to_shared_tools_maps_function_definition() {
    let tools = vec![make_tool(
        "search",
        serde_json::json!({"type": "object", "properties": {}}),
    )];
    let shared = to_shared_tools(&tools);
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].function.name, "search");
    assert_eq!(shared[0].function.description.as_deref(), Some("a tool"));
    assert_eq!(
        shared[0].function.parameters,
        Some(serde_json::json!({"type": "object", "properties": {}}))
    );
    assert!(
        shared[0].function.cache_control.is_none(),
        "FunctionObject has no cache_control field to carry"
    );
}

/// Why: every `agents::ToolChoice` variant must map onto its documented
/// `tci::ToolChoice` counterpart.
/// Test: itself.
#[test]
fn to_shared_tool_choice_maps_every_agent_variant() {
    assert_eq!(
        to_shared_tool_choice(AgentToolChoice::Auto),
        tci::ToolChoice::Auto
    );
    assert_eq!(
        to_shared_tool_choice(AgentToolChoice::Any),
        tci::ToolChoice::Required
    );
    assert_eq!(
        to_shared_tool_choice(AgentToolChoice::None),
        tci::ToolChoice::None
    );
}

/// The shared `tci::ToolChoice::Function` variant (unreachable from
/// `agents::ToolChoice`, which has no such variant) still maps to the correct
/// OpenAI-dialect wire shape via the shared crate's own mapper — proving the
/// full 4-variant seam works even though trusty-agents can only drive 3 of
/// them today.
/// Test: itself.
#[test]
fn shared_tool_choice_function_variant_maps_via_openai_tool_choice() {
    let v = tci::openai_tool_choice(tci::ToolChoice::Function("get_weather".into()));
    assert_eq!(
        v,
        serde_json::json!({"type": "function", "function": {"name": "get_weather"}})
    );
}

/// Why: content, tool calls, and usage must all flow through
/// `from_shared_response` into the tuple the tool loop expects.
/// Test: itself.
#[test]
fn from_shared_response_maps_content_tool_calls_and_usage() {
    let fixture = r#"{
      "id": "gen-1",
      "choices": [{"message": {"role": "assistant", "content": null,
          "tool_calls": [{"id": "c1", "type": "function",
              "function": {"name": "bash", "arguments": "{}"}}]},
          "finish_reason": "tool_calls"}],
      "usage": {"prompt_tokens": 12, "completion_tokens": 3,
                "prompt_tokens_details": {"cached_tokens": 4, "cache_write_tokens": 1}}
    }"#;
    let shared: tci::ChatResponse = serde_json::from_str(fixture).expect("deserialise");
    let (content, tool_calls, usage) = from_shared_response(shared);
    assert!(content.is_none());
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "c1");
    assert_eq!(tool_calls[0].function.name, "bash");
    assert_eq!(usage.prompt_tokens, 12);
    assert_eq!(usage.completion_tokens, 3);
    assert_eq!(usage.cache_read_tokens, 4);
    assert_eq!(usage.cache_creation_tokens, 1);
}

/// Why: a plain-text response (no tool calls) must round-trip cleanly too —
/// the common "final answer" turn.
/// Test: itself.
#[test]
fn from_shared_response_handles_no_tool_calls() {
    let fixture = r#"{
      "id": "gen-2",
      "choices": [{"message": {"role": "assistant", "content": "all done"},
                   "finish_reason": "stop"}],
      "usage": {"prompt_tokens": 5, "completion_tokens": 2}
    }"#;
    let shared: tci::ChatResponse = serde_json::from_str(fixture).expect("deserialise");
    let (content, tool_calls, usage) = from_shared_response(shared);
    assert_eq!(content.as_deref(), Some("all done"));
    assert!(tool_calls.is_empty());
    assert_eq!(usage.prompt_tokens, 5);
    assert_eq!(usage.completion_tokens, 2);
}
