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
//! vars both tests in this file mutate; both are `#[serial]` (unnamed group)
//! so they never interleave within this binary (each file under `tests/` is
//! its own process, so there is no cross-FILE race either), and
//! `InferenceClient`'s process-wide `shared_client()` `OnceLock` is
//! guaranteed to see whichever override was active at its first
//! initialisation.
//!
//! What: two tests —
//! 1. `bare_claude_model_routes_through_shared_adapter_when_flag_enabled`:
//!    routes a bare `claude-*` model through `chat_with_tools_gated` with the
//!    flag on, and asserts the parsed response AND the captured wire request
//!    (path, `Bearer` auth, qualified model id, detailed-usage directive).
//! 2. `caching_active_keeps_raw_path_even_with_flag_enabled`: the regression
//!    the plan calls the highest-value test in the epic — proves
//!    `enable_prompt_caching`'s `caching_active` term still forces
//!    `needs_raw` (`llm/tool_loop/mod.rs`'s `let needs_raw = caching_active ||
//!    …`) even when `TAGENT_INFERENCE_SHARED=1`, so a caching turn is NEVER
//!    silently routed through the shared adapter. See that test's own doc for
//!    why the wire signal is the OpenRouter-only `usage:{include:true}`
//!    directive's ABSENCE, not `cache_control`'s presence.
//! Test: this file (`cargo test -p trusty-agents --test inference_shared_adapter_e2e`).

use std::sync::Arc;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
};
use serde_json::json;
use serial_test::serial;

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
#[serial]
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
        false, // strict_tool_discipline (#3371): use_finish_task=false and
        // tool_choice=None (Auto), so LlmParams::strict_tool_discipline()'s
        // `use_finish_task || tool_choice == Any` is false — mirrors every
        // production call site's derivation for this same param combo.
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

/// Regression: a prompt-caching turn must keep taking the RAW dispatch path
/// — never the shared adapter — even with `TAGENT_INFERENCE_SHARED=1` set.
///
/// Why: `dispatch_turn`'s shared branch is reachable only when
/// `!routing.needs_raw` (`llm/tool_loop/mod.rs`: `let needs_raw =
/// caching_active || tool_choice.is_some() || route_native_anthropic ||
/// is_ollama;`). That's a plain boolean OR with no test pinning any one of
/// its terms — if a future refactor dropped or reordered `caching_active`,
/// prompt-caching traffic would silently start flowing through the shared
/// adapter with zero test failure and a real dollar cost (a dropped cache
/// discount). This test drives EXACTLY that term: `enable_prompt_caching =
/// true` on an Anthropic-family model with `use_anthropic_direct = false`
/// (so `route_native_anthropic` is false and does NOT independently force
/// `needs_raw` — only `caching_active` does here), which must resolve
/// `caching_active = true` and therefore `needs_raw = true`, sending the
/// turn down `send_raw_completion` instead of `dispatch_turn_shared`.
///
/// The observable wire signal is the ABSENCE of the OpenRouter-only
/// `usage:{include:true}` directive — NOT `cache_control`'s presence.
/// `dispatch_turn`'s raw branch only calls `adapter.inject_cache_control`
/// when `routing.route_native_anthropic` is true (see the `if
/// routing.route_native_anthropic { adapter.inject_cache_control(..) }` guard
/// inside the `needs_raw` block in `turn.rs`) — and `route_native_anthropic`
/// is deliberately false in this test so it isolates `caching_active` as the
/// sole thing forcing `needs_raw`. That's a pre-existing trusty-agents
/// behaviour (OpenRouter-routed, non-native-Anthropic caching has never
/// injected `cache_control` — the comment at that call site says injecting it
/// for OpenRouter breaks the OpenAI-format body OpenRouter expects) predating
/// and unrelated to #2410; this test does not change or depend on fixing it.
/// What `usage:{include:true}` DOES prove: only `inference_client`'s shared
/// `OpenAiCompatAdapter` ever injects that directive (`prepare_body`, driven
/// by the OpenRouter capability registry) — the raw path (`build_raw_request`)
/// and the legacy typed path (`CreateChatCompletionRequestArgs`) never set a
/// `usage` key at all. So `usage`'s absence is direct, wire-level proof this
/// request did NOT go through `dispatch_turn_shared` — the property that
/// would break with zero signal if `caching_active` fell out of the
/// `needs_raw` OR.
/// What: spawn a mock, point `OPENROUTER_BASE_URL` at it, enable the shared
/// flag, drive `chat_with_tools_gated` with `enable_prompt_caching = true`
/// and `use_anthropic_direct = false` on a `claude-*` model, then assert the
/// captured wire request has NO `usage` key.
/// Test: this test.
#[tokio::test]
#[serial]
async fn caching_active_keeps_raw_path_even_with_flag_enabled() {
    let server = MockInferenceServer::spawn(200, canned_response())
        .await
        .expect("spawn mock");

    // SAFETY: guarded by `#[serial]` against the sibling test in this file;
    // this is the only other test in this binary that touches these vars.
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

    let legacy_client = Client::with_config(OpenAIConfig::new());
    let model = "claude-sonnet-4-6"; // Anthropic-family -> caching_active eligible
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
        true,  // enable_prompt_caching — the term under test
        None,  // tool_choice
        false, // use_finish_task
        false, // strict_tool_discipline (#3371): use_finish_task=false and
        // tool_choice=None (Auto), so LlmParams::strict_tool_discipline()'s
        // `use_finish_task || tool_choice == Any` is false — mirrors every
        // production call site's derivation for this same param combo.
        false, // use_anthropic_direct — keeps route_native_anthropic false
        &[],   // stop_sequences
    )
    .await
    .expect("chat_with_tools_gated via raw path");

    assert_eq!(content, "pong-mock");

    let captured = server.last_request().expect("one request captured");
    assert_eq!(
        captured.path, "/chat/completions",
        "the raw path posts to the same OpenAI-compatible endpoint as the shared/legacy paths"
    );
    let body = captured.body.expect("json body");
    assert!(
        body.get("usage").is_none(),
        "a caching-active turn must take the raw path, which never sets `usage` — \
         its presence would mean the shared adapter (the only thing that injects \
         `usage:{{include:true}}`) handled this call instead, i.e. `caching_active` \
         stopped forcing `needs_raw`. Body: {body}"
    );

    unsafe {
        std::env::remove_var(SHARED_INFERENCE_ENV);
        std::env::remove_var("OPENROUTER_BASE_URL");
        std::env::remove_var("OPENROUTER_API_KEY");
    }
}
