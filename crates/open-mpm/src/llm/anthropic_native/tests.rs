//! Unit tests for the Anthropic native request/response shim.
//!
//! Why: Keeping the test suite in its own file holds both this and the logic
//! module under the 500-line cap while preserving full coverage.
//! What: Covers request building (system lifting, prefix stripping, tool
//! schema conversion, cache breakpoints, tool_result conversion) and response
//! parsing (text/tool_use extraction, cache-aware usage, stop-reason default).
//! Test: This IS the test module.

use super::convert::{
    attach_cache_control_to_last_assistant, history_token_estimate, strip_provider_prefix,
};
use super::*;
use async_openai::types::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessageArgs,
    ChatCompletionToolArgs, FunctionObjectArgs,
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

fn assistant_tool_call(id: &str, name: &str, args: &str) -> ChatCompletionRequestMessage {
    let call = ChatCompletionMessageToolCall {
        id: id.into(),
        r#type: ChatCompletionToolType::Function,
        function: FunctionCall {
            name: name.into(),
            arguments: args.into(),
        },
    };
    ChatCompletionRequestAssistantMessageArgs::default()
        .tool_calls(vec![call])
        .build()
        .unwrap()
        .into()
}

fn make_tool(name: &str, params: Value) -> ChatCompletionTool {
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

#[test]
fn strip_provider_prefix_cases() {
    assert_eq!(
        strip_provider_prefix("anthropic/claude-sonnet-4-5"),
        "claude-sonnet-4-5"
    );
    assert_eq!(strip_provider_prefix("claude-haiku-4"), "claude-haiku-4");
    assert_eq!(strip_provider_prefix(""), "");
}

#[test]
fn build_anthropic_request_places_system_top_level() {
    let messages = vec![sys("You are helpful."), user("Hi there")];
    let body = build_anthropic_request(
        "anthropic/claude-sonnet-4-5",
        &messages,
        &[],
        0.2,
        1024,
        None,
        false,
    )
    .unwrap();
    assert_eq!(body["system"], json!("You are helpful."));
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "system should be lifted out of messages");
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn build_anthropic_request_strips_provider_prefix_from_model() {
    let body = build_anthropic_request(
        "anthropic/claude-sonnet-4-5",
        &[user("hi")],
        &[],
        0.0,
        100,
        None,
        false,
    )
    .unwrap();
    assert_eq!(body["model"], json!("claude-sonnet-4-5"));
}

#[test]
fn build_anthropic_request_converts_tool_parameters_to_input_schema() {
    let params = json!({
        "type": "object",
        "properties": {"x": {"type": "string"}},
        "required": ["x"]
    });
    let tool = make_tool("do_thing", params.clone());
    let body = build_anthropic_request(
        "claude-sonnet-4-5",
        &[sys("s"), user("u")],
        &[tool],
        0.0,
        100,
        None,
        false,
    )
    .unwrap();
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], json!("do_thing"));
    assert_eq!(tools[0]["input_schema"], params);
    assert!(
        tools[0].get("parameters").is_none(),
        "native request must not carry OpenAI 'parameters' key"
    );
}

#[test]
fn build_anthropic_request_caching_marks_system_and_last_tool() {
    let tool = make_tool("t", json!({"type":"object","properties":{}}));
    let body = build_anthropic_request(
        "claude-sonnet-4-5",
        &[sys("s"), user("u")],
        &[tool],
        0.0,
        100,
        None,
        true, // enable_caching
    )
    .unwrap();
    // System should now be a content-block array with cache_control.
    let sys_arr = body["system"].as_array().expect("system as array");
    assert_eq!(sys_arr[0]["cache_control"]["type"], "ephemeral");
    // Last tool should carry cache_control.
    let last_tool = body["tools"].as_array().unwrap().last().unwrap();
    assert_eq!(last_tool["cache_control"]["type"], "ephemeral");
}

#[test]
fn build_anthropic_request_converts_tool_result_messages() {
    let messages = vec![
        sys("s"),
        user("please search"),
        assistant_tool_call("toolu_1", "search", r#"{"q":"rust"}"#),
        tool_msg("toolu_1", "found 42 results"),
    ];
    let body = build_anthropic_request("claude-sonnet-4-5", &messages, &[], 0.0, 100, None, false)
        .unwrap();
    let msgs = body["messages"].as_array().unwrap();
    // Expected: [user("please search"), assistant(tool_use), user(tool_result)]
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["role"], "assistant");
    let assistant_blocks = msgs[1]["content"].as_array().unwrap();
    assert!(
        assistant_blocks
            .iter()
            .any(|b| b["type"] == "tool_use" && b["id"] == "toolu_1"),
        "missing tool_use block: {assistant_blocks:?}"
    );
    assert_eq!(msgs[2]["role"], "user");
    let result_blocks = msgs[2]["content"].as_array().unwrap();
    assert_eq!(result_blocks[0]["type"], "tool_result");
    assert_eq!(result_blocks[0]["tool_use_id"], "toolu_1");
    assert_eq!(result_blocks[0]["content"], "found 42 results");
}

#[test]
fn build_anthropic_request_passes_tool_choice_through() {
    let body = build_anthropic_request(
        "claude-sonnet-4-5",
        &[user("hi")],
        &[],
        0.0,
        100,
        Some(&json!({"type": "any"})),
        false,
    )
    .unwrap();
    assert_eq!(body["tool_choice"], json!({"type": "any"}));
}

#[test]
fn parse_anthropic_response_extracts_text_and_tool_use() {
    let resp = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "Working on it."},
            {"type": "tool_use", "id": "toolu_abc", "name": "finish_task",
             "input": {"summary": "done"}}
        ],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 100, "output_tokens": 50}
    });
    let parsed = parse_anthropic_response(&resp);
    assert_eq!(parsed.text_content.as_deref(), Some("Working on it."));
    assert_eq!(parsed.stop_reason, "tool_use");
    assert_eq!(parsed.tool_calls.len(), 1);
    assert_eq!(parsed.tool_calls[0].id, "toolu_abc");
    assert_eq!(parsed.tool_calls[0].function.name, "finish_task");
    let args: Value = serde_json::from_str(&parsed.tool_calls[0].function.arguments).unwrap();
    assert_eq!(args["summary"], json!("done"));
    assert_eq!(parsed.usage.prompt_tokens, 100);
    assert_eq!(parsed.usage.completion_tokens, 50);
}

#[test]
fn parse_anthropic_response_reads_cache_read_tokens() {
    let resp = json!({
        "content": [{"type":"text","text":"hi"}],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_input_tokens": 80,
            "cache_creation_input_tokens": 3
        }
    });
    let parsed = parse_anthropic_response(&resp);
    assert_eq!(parsed.usage.cache_read_tokens, 80);
    assert_eq!(parsed.usage.cache_creation_tokens, 3);
}

#[test]
fn parse_anthropic_response_empty_content_yields_none() {
    let resp = json!({"content": [], "stop_reason": "end_turn"});
    let parsed = parse_anthropic_response(&resp);
    assert!(parsed.text_content.is_none());
    assert!(parsed.tool_calls.is_empty());
    assert_eq!(parsed.stop_reason, "end_turn");
}

#[test]
fn parse_anthropic_response_defaults_stop_reason() {
    let resp = json!({"content": [{"type":"text","text":"x"}]});
    let parsed = parse_anthropic_response(&resp);
    assert_eq!(parsed.stop_reason, "end_turn");
}

// --- #29: Conversation-history cache breakpoint tests ---

#[test]
fn history_token_estimate_counts_text_blocks() {
    // ~ chars/4 token estimate; 8000 chars -> ~2000 tokens.
    let msgs = vec![
        json!({"role":"user","content":"hello"}),
        json!({"role":"assistant","content":[{"type":"text","text":"a".repeat(8000)}]}),
    ];
    let est = history_token_estimate(&msgs);
    assert!(est >= 2000, "expected >=2000, got {est}");
}

#[test]
fn attach_cache_control_to_last_assistant_inserts_marker() {
    let mut msgs = vec![
        json!({"role":"user","content":"hi"}),
        json!({"role":"assistant","content":"first reply"}),
        json!({"role":"user","content":"thanks"}),
        json!({"role":"assistant","content":[{"type":"text","text":"second reply"}]}),
    ];
    attach_cache_control_to_last_assistant(&mut msgs);
    // Last assistant message is index 3; its content[0] should now carry cache_control.
    let blocks = msgs[3]["content"].as_array().expect("array");
    assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    // Earlier assistant message (index 1) must NOT have been modified.
    // (still a String content)
    assert!(msgs[1]["content"].is_string());
}

#[test]
fn build_anthropic_request_caches_last_assistant_when_large() {
    // #29: A long assistant turn (>2000 tokens, ~8000 chars) should trigger
    // a second cache breakpoint on the most recent assistant block.
    let big = "x".repeat(10_000);
    let assistant: ChatCompletionRequestMessage =
        ChatCompletionRequestAssistantMessageArgs::default()
            .content(big.as_str())
            .build()
            .unwrap()
            .into();
    let msgs = vec![sys("you are helpful"), user("hi"), assistant];
    let body = build_anthropic_request(
        "anthropic/claude-sonnet-4-6",
        &msgs,
        &[],
        0.2,
        1024,
        None,
        true,
    )
    .unwrap();
    let messages = body["messages"].as_array().expect("messages");
    // The last message is the assistant turn — its last block should have cache_control.
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "assistant");
    let content = last["content"].as_array().expect("array content");
    let last_block = content.last().unwrap();
    assert_eq!(
        last_block["cache_control"]["type"], "ephemeral",
        "expected cache_control on last assistant block: {body:#}"
    );
}

#[test]
fn build_anthropic_request_skips_history_cache_when_small() {
    // #29: Below the 2000-token threshold we must NOT emit a second
    // breakpoint — only the system prompt gets cached.
    let assistant: ChatCompletionRequestMessage =
        ChatCompletionRequestAssistantMessageArgs::default()
            .content("short reply")
            .build()
            .unwrap()
            .into();
    let msgs = vec![sys("sys"), user("hi"), assistant];
    let body = build_anthropic_request(
        "anthropic/claude-sonnet-4-6",
        &msgs,
        &[],
        0.2,
        1024,
        None,
        true,
    )
    .unwrap();
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    // Either still a string content, or an array without cache_control.
    match &last["content"] {
        Value::String(_) => {}
        Value::Array(a) => {
            for b in a {
                assert!(
                    b.get("cache_control").is_none(),
                    "small history must not emit cache_control: {b}"
                );
            }
        }
        _ => {}
    }
}

#[test]
fn build_anthropic_request_skips_history_cache_when_caching_disabled() {
    // #29: Even with a long history, enable_caching=false must not emit
    // any cache_control markers — caching is opt-in via the agent config.
    let big = "x".repeat(10_000);
    let assistant: ChatCompletionRequestMessage =
        ChatCompletionRequestAssistantMessageArgs::default()
            .content(big.as_str())
            .build()
            .unwrap()
            .into();
    let msgs = vec![sys("sys"), user("hi"), assistant];
    let body = build_anthropic_request(
        "anthropic/claude-sonnet-4-6",
        &msgs,
        &[],
        0.2,
        1024,
        None,
        false, // caching disabled
    )
    .unwrap();
    // System should be a plain string (no cache_control wrapping).
    assert!(body["system"].is_string());
    // Last assistant message must not have cache_control either.
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    if let Some(arr) = last["content"].as_array() {
        for b in arr {
            assert!(b.get("cache_control").is_none());
        }
    }
}
