//! Anthropic Messages-API request translation (issue #2408, epic #2400 Wave 2).
//!
//! Why: the Anthropic first-party `/v1/messages` wire format is NOT the OpenAI
//! `/chat/completions` schema the shared [`super::super::openai_compat`] core
//! speaks — `system` is a top-level parameter (not a message role), tool results
//! are `tool_result` content blocks inside a `user` turn (not a `tool` role),
//! tools are `{name, description, input_schema}` (not `{type, function}`),
//! `max_tokens` is REQUIRED, and stop sequences ride `stop_sequences` (not
//! `stop`). Shoehorning the neutral [`ChatRequest`] through OpenAI serde would
//! silently drop the system prompt and mis-shape every tool. This module owns the
//! neutral→Anthropic translation as pure, testable functions.
//! What: [`build_body`] converts a [`ChatRequest`] into the Anthropic request
//! `Value` (hoisting `system`, coalescing tool results into `user` turns, honoring
//! `cache_control` breakpoints on system/message/tool blocks per #2156), and
//! [`anthropic_tool_choice`] maps the neutral [`ToolChoice`] into the Anthropic
//! `tool_choice` dialect.
//! Test: inline `tests` — `hoists_system_and_defaults_max_tokens`,
//! `builds_alternating_user_assistant_turns`, `tool_result_becomes_user_turn`,
//! `converts_tools_to_input_schema`, `cache_control_marks_system_and_blocks`,
//! `strips_anthropic_prefix`, `anthropic_tool_choice_maps_every_variant`.

use serde_json::{Map, Value, json};

use crate::inference::types::{ChatRequest, ToolChoice, ToolDefinition};

/// Convert a neutral [`ChatRequest`] into an Anthropic `/v1/messages` body.
///
/// Why: the adapter's [`super::AnthropicAdapter::chat`] must POST the exact
/// Anthropic wire shape; keeping the translation a pure function makes every
/// structural rule (system hoist, role remap, cache markers, required
/// `max_tokens`) unit-testable without a socket.
/// What: emits `model` (with any leading `anthropic/` routing prefix stripped),
/// `max_tokens` (the request's value or `default_max_tokens`, since Anthropic
/// requires it), the `messages` array (system messages hoisted to the top-level
/// `system` param; `tool` messages remapped to `user` `tool_result` blocks;
/// consecutive same-role turns coalesced), and the optional `system`,
/// `temperature`, `stop_sequences`, `tools`, and `tool_choice` fields. Any
/// `cache_control` breakpoint on a message/tool is rendered as
/// `{"type":"ephemeral"}` on the corresponding block.
/// Test: `hoists_system_and_defaults_max_tokens`,
/// `builds_alternating_user_assistant_turns`, `tool_result_becomes_user_turn`.
pub fn build_body(request: &ChatRequest, default_max_tokens: u32) -> Value {
    let mut system_blocks: Vec<Value> = Vec::new();
    let mut system_has_cache = false;
    // (role, blocks) conversation turns, pre-coalescing.
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();

    for msg in &request.messages {
        match msg.role.as_str() {
            "system" => {
                if let Some(text) = &msg.content {
                    let mut block = json!({ "type": "text", "text": text });
                    if msg.cache_control.is_some() {
                        system_has_cache = true;
                        block["cache_control"] = json!({ "type": "ephemeral" });
                    }
                    system_blocks.push(block);
                }
            }
            "tool" => {
                // Anthropic tool results are `tool_result` blocks in a USER turn.
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                    "content": msg.content.clone().unwrap_or_default(),
                });
                if msg.cache_control.is_some() {
                    block["cache_control"] = json!({ "type": "ephemeral" });
                }
                push_turn(&mut turns, "user", vec![block]);
            }
            role => {
                let is_assistant = role == "assistant";
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = &msg.content {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                if let Some(calls) = msg.tool_calls.as_ref().filter(|_| is_assistant) {
                    for call in calls {
                        let input: Value = serde_json::from_str(&call.function.arguments)
                            .unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.function.name,
                            "input": input,
                        }));
                    }
                }
                if blocks.is_empty() {
                    // An assistant turn with neither text nor tool calls has no
                    // Anthropic content block — skip it rather than emit an empty
                    // (and API-rejected) content array.
                    continue;
                }
                // Cache breakpoint attaches to this message's LAST block.
                if let Some(last) = blocks.last_mut().filter(|_| msg.cache_control.is_some()) {
                    last["cache_control"] = json!({ "type": "ephemeral" });
                }
                let anth_role = if is_assistant { "assistant" } else { "user" };
                push_turn(&mut turns, anth_role, blocks);
            }
        }
    }

    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, blocks)| json!({ "role": role, "content": blocks }))
        .collect();

    let mut body = Map::new();
    body.insert("model".into(), json!(strip_prefix(&request.model)));
    body.insert(
        "max_tokens".into(),
        json!(request.max_tokens.unwrap_or(default_max_tokens)),
    );
    body.insert("messages".into(), Value::Array(messages));

    if !system_blocks.is_empty() {
        if system_has_cache {
            // A cache breakpoint can only be expressed on the content-block form.
            body.insert("system".into(), Value::Array(system_blocks));
        } else {
            let joined = system_blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            body.insert("system".into(), json!(joined));
        }
    }
    if let Some(t) = request.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(stop) = &request.stop {
        body.insert("stop_sequences".into(), json!(stop));
    }
    if let Some(tools) = &request.tools {
        body.insert("tools".into(), Value::Array(convert_tools(tools)));
    }
    if let Some(tc) = &request.tool_choice {
        body.insert("tool_choice".into(), tc.clone());
    }
    Value::Object(body)
}

/// Map the neutral [`ToolChoice`] into the Anthropic `tool_choice` wire value.
///
/// Why: Anthropic spells tool-choice differently from OpenAI — `any` (not
/// `required`) forces some call, a specific call is `{"type":"tool","name":…}`
/// (not a nested `function` object), and there is a first-class `{"type":"none"}`.
/// The adapter overrides [`crate::inference::InferenceAdapter::map_tool_choice`]
/// with this so callers stay dialect-agnostic.
/// What: `None`→`{"type":"none"}`, `Auto`→`{"type":"auto"}`,
/// `Required`→`{"type":"any"}`, `Function(name)`→`{"type":"tool","name":name}`.
/// Test: `anthropic_tool_choice_maps_every_variant`.
pub fn anthropic_tool_choice(choice: ToolChoice) -> Value {
    match choice {
        ToolChoice::None => json!({ "type": "none" }),
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::Required => json!({ "type": "any" }),
        ToolChoice::Function(name) => json!({ "type": "tool", "name": name }),
    }
}

/// Append `blocks` to the previous turn when it shares `role`, else push a new
/// turn.
///
/// Why: Anthropic requires strictly alternating `user`/`assistant` turns; a
/// sequence of tool results (each a `user` `tool_result` block) plus a preceding
/// user message must collapse into ONE user turn or the API rejects consecutive
/// same-role messages.
/// What: mutates `turns`, coalescing adjacent same-role block lists.
/// Test: `tool_result_becomes_user_turn`.
fn push_turn(
    turns: &mut Vec<(&'static str, Vec<Value>)>,
    role: &'static str,
    mut blocks: Vec<Value>,
) {
    match turns.last_mut() {
        Some(last) if last.0 == role => last.1.append(&mut blocks),
        _ => turns.push((role, blocks)),
    }
}

/// Convert neutral [`ToolDefinition`]s into Anthropic tool objects.
///
/// Why: Anthropic tools are `{name, description, input_schema}` with an optional
/// `cache_control` breakpoint — not the OpenAI `{type:"function", function:{…}}`
/// envelope; the schema key is `input_schema`, not `parameters`.
/// What: maps each function tool, defaulting a missing schema to
/// `{"type":"object"}` and forwarding a `cache_control` marker when set.
/// Test: `converts_tools_to_input_schema`, `cache_control_marks_system_and_blocks`.
fn convert_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let f = &t.function;
            let mut tool = Map::new();
            tool.insert("name".into(), json!(f.name));
            if let Some(desc) = &f.description {
                tool.insert("description".into(), json!(desc));
            }
            tool.insert(
                "input_schema".into(),
                f.parameters
                    .clone()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            );
            if f.cache_control.is_some() {
                tool.insert("cache_control".into(), json!({ "type": "ephemeral" }));
            }
            Value::Object(tool)
        })
        .collect()
}

/// Strip a leading `anthropic/` routing prefix from a model slug.
///
/// Why: the resolver preserves the explicit `anthropic/claude-…` slug so the
/// two-stage resolver can route it, but the Anthropic API expects the bare model
/// id (`claude-…`); sending the prefixed form is an unknown-model 404.
/// What: removes a leading `anthropic/` if present, else returns the slug as-is.
/// Test: `strips_anthropic_prefix`.
fn strip_prefix(model: &str) -> &str {
    model.strip_prefix("anthropic/").unwrap_or(model)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::types::CacheControl;
    use crate::inference::types::{
        ChatMessage, FunctionCall, FunctionDefinition, ToolCall, ToolDefinition,
    };

    /// Why: `system` must be hoisted out of the message list to the top-level
    /// param, and `max_tokens` must always be present (Anthropic requires it),
    /// defaulting when the caller left it unset.
    /// Test: itself.
    #[test]
    fn hoists_system_and_defaults_max_tokens() {
        let req = ChatRequest::new(
            "claude-sonnet-4-5",
            vec![ChatMessage::system("be terse"), ChatMessage::user("hi")],
        );
        let body = build_body(&req, 4096);
        assert_eq!(body["system"], json!("be terse"));
        assert_eq!(body["max_tokens"], json!(4096));
        // The system message is NOT in the messages array.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
    }

    /// Why: user and assistant turns must serialise as content-block arrays in
    /// order, and an explicit `max_tokens` must be honored over the default.
    /// Test: itself.
    #[test]
    fn builds_alternating_user_assistant_turns() {
        let mut req = ChatRequest::new(
            "claude-sonnet-4-5",
            vec![
                ChatMessage::user("q1"),
                ChatMessage::assistant("a1"),
                ChatMessage::user("q2"),
            ],
        );
        req.max_tokens = Some(256);
        req.temperature = Some(0.2);
        req.stop = Some(vec!["STOP".into()]);
        let body = build_body(&req, 4096);
        assert_eq!(body["max_tokens"], json!(256));
        assert_eq!(body["temperature"], json!(0.2_f32));
        assert_eq!(body["stop_sequences"], json!(["STOP"]));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["text"], "a1");
        assert_eq!(msgs[2]["role"], "user");
    }

    /// Why: a `tool` role message must remap to a `user` turn carrying a
    /// `tool_result` block, and consecutive user turns (a user message followed
    /// by a tool result) must coalesce into one turn — Anthropic forbids adjacent
    /// same-role messages.
    /// Test: itself.
    #[test]
    fn tool_result_becomes_user_turn() {
        let assistant_call = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "toolu_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "get_weather".into(),
                    arguments: r#"{"loc":"SEA"}"#.into(),
                },
            }]),
            tool_call_id: None,
            name: None,
            cache_control: None,
        };
        let req = ChatRequest::new(
            "claude-sonnet-4-5",
            vec![
                ChatMessage::user("weather?"),
                assistant_call,
                ChatMessage::tool_result("toolu_1", "get_weather", r#"{"t":72}"#),
                ChatMessage::tool_result("toolu_2", "get_time", "noon"),
            ],
        );
        let body = build_body(&req, 4096);
        let msgs = body["messages"].as_array().unwrap();
        // user, assistant(tool_use), user(2 coalesced tool_results)
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][0]["id"], "toolu_1");
        assert_eq!(msgs[1]["content"][0]["input"]["loc"], "SEA");
        assert_eq!(msgs[2]["role"], "user");
        let results = msgs[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "toolu_1");
        assert_eq!(results[1]["tool_use_id"], "toolu_2");
    }

    /// Why: tools must serialise as `{name, input_schema}` (Anthropic's key),
    /// with a default object schema when the caller supplied none, and the
    /// caller-supplied `tool_choice` value must pass through verbatim.
    /// Test: itself.
    #[test]
    fn converts_tools_to_input_schema() {
        let mut req = ChatRequest::new("claude-sonnet-4-5", vec![ChatMessage::user("hi")]);
        req.tools = Some(vec![ToolDefinition::function(FunctionDefinition {
            name: "get_weather".into(),
            description: Some("look up weather".into()),
            parameters: Some(json!({"type": "object", "properties": {}})),
            cache_control: None,
        })]);
        req.tool_choice = Some(anthropic_tool_choice(ToolChoice::Auto));
        let body = build_body(&req, 4096);
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(body["tools"][0]["description"], "look up weather");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        // No `parameters`/`function` OpenAI envelope leaks through.
        assert!(body["tools"][0].get("parameters").is_none());
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"], json!({"type": "auto"}));
    }

    /// Why: `cache_control` breakpoints (#2156) must render as
    /// `{"type":"ephemeral"}` on the system block, a message's last content
    /// block, and a tool — driving the `system` param to its array form.
    /// Test: itself.
    #[test]
    fn cache_control_marks_system_and_blocks() {
        let mut sys = ChatMessage::system("stable prefix");
        sys.cache_control = Some(CacheControl::ephemeral());
        let mut user = ChatMessage::user("cache me too");
        user.cache_control = Some(CacheControl::ephemeral());
        let mut req = ChatRequest::new("claude-sonnet-4-5", vec![sys, user]);
        req.tools = Some(vec![ToolDefinition::function(FunctionDefinition {
            name: "t".into(),
            description: None,
            parameters: None,
            cache_control: Some(CacheControl::ephemeral()),
        })]);
        let body = build_body(&req, 4096);
        // system rendered as a block array with a cache marker.
        assert_eq!(
            body["system"],
            json!([{ "type": "text", "text": "stable prefix", "cache_control": {"type": "ephemeral"} }])
        );
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(
            body["tools"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    /// Why: the explicit `anthropic/` routing prefix must be stripped so the API
    /// receives the bare model id.
    /// Test: itself.
    #[test]
    fn strips_anthropic_prefix() {
        let req = ChatRequest::new("anthropic/claude-sonnet-4-5", vec![ChatMessage::user("x")]);
        let body = build_body(&req, 4096);
        assert_eq!(body["model"], "claude-sonnet-4-5");
    }

    /// Why: every neutral tool-choice variant must map to its Anthropic spelling;
    /// a typo would silently break tool routing.
    /// Test: itself.
    #[test]
    fn anthropic_tool_choice_maps_every_variant() {
        assert_eq!(
            anthropic_tool_choice(ToolChoice::None),
            json!({"type": "none"})
        );
        assert_eq!(
            anthropic_tool_choice(ToolChoice::Auto),
            json!({"type": "auto"})
        );
        assert_eq!(
            anthropic_tool_choice(ToolChoice::Required),
            json!({"type": "any"})
        );
        assert_eq!(
            anthropic_tool_choice(ToolChoice::Function("get_weather".into())),
            json!({"type": "tool", "name": "get_weather"})
        );
    }
}
