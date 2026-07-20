//! Black-box e2e: prove trusty-agents' OpenRouter traffic flows through the
//! shared `trusty_common::inference` adapter when `TAGENT_INFERENCE_SHARED=1`
//! (#2410, epic #2400, Step 1+2).
//!
//! Why: #2410's core claim is that, behind a default-OFF flag, trusty-agents'
//! plain OpenRouter dispatch path (`llm::tool_loop::turn::dispatch_turn`'s
//! typed branch) can be routed through the shared adapter with byte-identical
//! wire behaviour to the legacy `async-openai` client — including the
//! `usage:{include:true}` detailed-usage directive OpenRouter only returns
//! when asked, and the `qualify_openrouter_model` bare-`claude-*` → `anthropic/
//! claude-*` normalisation. This drives the crate's REAL public entry point
//! (`trusty_agents::llm::chat_with_tools_gated`, the same function every agent
//! run calls) over a REAL loopback socket served by the shared
//! `test_support::MockInferenceServer`, then asserts on the exact wire request
//! the shared adapter emitted. That request shape is producible ONLY by the
//! shared adapter having actually been invoked — so a green run is direct
//! evidence Step 2's wiring works end-to-end, not just at the unit level.
//!
//! `OPENROUTER_BASE_URL` and `TAGENT_INFERENCE_SHARED` are process-global env
//! vars this file mutates; this is the only test function in this binary
//! (each file under `tests/` is its own process) so there is no cross-test
//! race, and `InferenceClient`'s process-wide `shared_client()` `OnceLock` is
//! guaranteed to see the override on its first (and only) initialisation.
//!
//! What: one test — routes a bare `claude-*` model through
//! `chat_with_tools_gated` with the flag on, and asserts the parsed response
//! AND the captured wire request (path, `Bearer` auth, qualified model id,
//! detailed-usage directive).
//! Test: this file (`cargo test -p trusty-agents --test inference_shared_adapter_e2e`).

use std::sync::Arc;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
};
use serde_json::json;

use trusty_agents::llm::adapter::adapter_for_model;
use trusty_agents::llm::chat_with_tools_gated;
use trusty_agents::llm::inference_client::SHARED_INFERENCE_ENV;
use trusty_agents::tools::ToolRegistry;
use trusty_common::inference::test_support::MockInferenceServer;

/// A canned OpenAI-compatible chat/completions response the mock serves.
fn canned_response() -> serde_json::Value {
    json!({
        "id": "gen-mock",
        "model": "anthropic/claude-sonnet-4-6",
        "choices": [{
            "message": {"role": "assistant", "content": "pong-mock", "tool_calls": []},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
    })
}

/// A bare-`claude-*` model (unqualified) routed with `TAGENT_INFERENCE_SHARED=1`
/// flows through the shared OpenRouter adapter: the qualifier prefixes the
/// model, the shared adapter injects the detailed-usage directive, and the
/// response comes back through the same `(content, tool_calls, usage)` shape
/// the legacy path produces.
///
/// Why: proves Step 2's wiring end-to-end through the crate's real public
/// entry point, not a synthetic call into `inference_client` directly.
/// What: point `OPENROUTER_BASE_URL` at the mock, set the flag, drive
/// `chat_with_tools_gated` for one turn (no tools, so the model's plain-text
/// reply ends the loop immediately), then assert the parsed content AND the
/// captured wire request.
/// Test: this test.
#[tokio::test]
async fn bare_claude_model_routes_through_shared_adapter_when_flag_enabled() {
    let server = MockInferenceServer::spawn(200, canned_response())
        .await
        .expect("spawn mock");

    // SAFETY: this file is the only test in this binary (integration test
    // files each get their own process), so there is no cross-test race on
    // these process-global env vars.
    unsafe {
        std::env::set_var("OPENROUTER_BASE_URL", server.url());
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-mock"); // pragma: allowlist secret
        std::env::set_var(SHARED_INFERENCE_ENV, "1");
    }

    let messages: Vec<ChatCompletionRequestMessage> = vec![
        ChatCompletionRequestSystemMessageArgs::default()
            .content("You are a concise assistant.")
            .build()
            .unwrap()
            .into(),
        ChatCompletionRequestUserMessageArgs::default()
            .content("Reply with exactly the word: pong")
            .build()
            .unwrap()
            .into(),
    ];

    // The legacy client is never touched on the shared path (see
    // `dispatch_turn`'s flag gate), so an unconfigured placeholder is fine.
    let legacy_client = Client::with_config(OpenAIConfig::new());
    let model = "claude-sonnet-4-6"; // bare — must be qualified to anthropic/claude-sonnet-4-6
    let adapter = adapter_for_model(model);
    let registry = Arc::new(ToolRegistry::new());

    let (content, _usage) = chat_with_tools_gated(
        &legacy_client,
        model,
        &*adapter,
        messages,
        registry,
        None,  // allowed_tools
        0.0,   // temperature
        16,    // max_tokens
        1,     // max_turns
        false, // enable_prompt_caching
        None,  // tool_choice
        false, // use_finish_task
        false, // use_anthropic_direct
        &[],   // stop_sequences
    )
    .await
    .expect("chat_with_tools_gated via shared adapter");

    assert_eq!(content, "pong-mock");

    // The shared adapter's outbound wire request — this shape is producible
    // ONLY by the shared adapter having actually handled the call.
    let captured = server.last_request().expect("one request captured");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/chat/completions");
    assert!(
        captured
            .header("authorization")
            .is_some_and(|v| v.starts_with("Bearer ")),
        "shared adapter must send a Bearer auth header"
    );
    let body = captured.body.expect("json body");
    assert_eq!(
        body["model"], "anthropic/claude-sonnet-4-6",
        "qualify_openrouter_model must prefix the bare claude-* id"
    );
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(
        body["usage"],
        json!({"include": true}),
        "shared OpenRouter adapter must inject the detailed-usage directive"
    );

    unsafe {
        std::env::remove_var(SHARED_INFERENCE_ENV);
        std::env::remove_var("OPENROUTER_BASE_URL");
        std::env::remove_var("OPENROUTER_API_KEY");
    }
}
