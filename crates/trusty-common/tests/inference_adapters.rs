//! Offline e2e for the concrete inference adapters (issue #2403, epic #2400).
//!
//! Why: the acceptance criteria demand each adapter be driven through the full
//! `Configurator` resolve→build→`chat`→`usage` cycle against a REAL socket
//! ([`MockInferenceServer`]) — asserting BOTH the outbound request translation
//! (the mock captures the body + headers the adapter sent) AND the response /
//! usage parsing, without a live provider or network. This is the cycle #2402's
//! `inference_foundation.rs` left as a placeholder ("where #2403's live-provider
//! e2e attaches"); this file realises it for OpenRouter, Fireworks, and OpenAI.
//! What: black-box tests against `trusty_common::inference::*`, pointing each
//! provider's adapter core at the mock's URL through a `Configurator` factory.
//! Requires `inference-client` (adapters) + `axum-server` (the mock).
//! Test: this file — run with
//! `cargo test -p trusty-common --features inference-client,axum-server`.

use serde_json::{Value, json};
use serial_test::serial;
use trusty_common::inference::credentials::{KeyStore, MemoryKeyStore};
use trusty_common::inference::providers::{
    anthropic, atlascloud, fireworks, openai, openrouter, together,
};
use trusty_common::inference::test_support::MockInferenceServer;
use trusty_common::inference::{
    CacheControl, ChatMessage, ChatRequest, Configurator, FunctionDefinition, InferenceError,
    ProviderId, ResolvedProvider, ToolChoice, ToolDefinition, register_default_factories,
};

/// Clear every provider env var so the injected `MemoryKeyStore` is the only
/// credential source (a stray `OPENROUTER_API_KEY` would otherwise shadow it).
fn clear_provider_env() {
    for var in [
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "FIREWORKS_API_KEY",
        "TOGETHER_API_KEY",
        "ATLASCLOUD_API_KEY",
    ] {
        // SAFETY: every caller is `#[serial(dotenv_credential_env)]`, matching
        // the lock the credential resolver's own env-mutating tests use.
        unsafe { std::env::remove_var(var) };
    }
}

/// A minimal successful chat/completions response body (text turn).
fn text_response_body() -> Value {
    json!({
        "id": "gen-mock",
        "model": "served/model",
        "choices": [{
            "message": {"role": "assistant", "content": "pong"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 7, "completion_tokens": 1, "total_tokens": 8}
    })
}

// ── OpenRouter: request translation + detailed-usage directive ───────────────────

/// Why: driving OpenRouter through the `Configurator` must (a) send an
/// OpenAI-shaped body with the model + messages, (b) inject `usage:{include:true}`
/// (OpenRouter's detailed-usage flag), (c) attach the `Authorization` bearer and
/// attribution headers, and (d) parse the response text back.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn openrouter_translates_request_and_parses_response() {
    clear_provider_env();
    let server = MockInferenceServer::spawn(200, text_response_body())
        .await
        .expect("spawn mock");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").unwrap(); // pragma: allowlist secret

    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(move |r: &ResolvedProvider| openrouter::build(r, &base)),
    );

    let adapter = cfg.build("openai/gpt-4o-mini", &store).expect("build");
    assert_eq!(adapter.name(), "openrouter");

    let req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("ping")]);
    let resp = adapter.chat(&req).await.expect("chat ok");

    // Response parsing.
    assert_eq!(resp.first_text().as_deref(), Some("pong"));
    assert_eq!(resp.usage().total_tokens(), 8);

    // Request translation captured by the mock.
    let sent = server.last_request().expect("captured request");
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/chat/completions");
    assert_eq!(sent.header("authorization"), Some("Bearer sk-or-fake")); // pragma: allowlist secret
    assert_eq!(
        sent.header("http-referer"),
        Some("https://github.com/bobmatnyc/trusty-tools")
    );
    assert_eq!(sent.header("x-title"), Some("trusty-tools"));
    let body = sent.body.expect("json body");
    assert_eq!(body["model"], "openai/gpt-4o-mini");
    assert_eq!(body["messages"][0]["content"], "ping");
    // OpenRouter detailed-usage directive injected by the core.
    assert_eq!(body["usage"], json!({"include": true}));
}

/// Why: the cache-token accounting path must parse OpenRouter's nested
/// `prompt_tokens_details` into the normalized `Usage` cache buckets, and a
/// caller-set `cache_control` breakpoint must pass through onto the wire.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn openrouter_cache_accounting_and_passthrough() {
    clear_provider_env();
    let body = json!({
        "id": "gen-cache",
        "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": 1200,
            "completion_tokens": 40,
            "cost": 0.0021,
            "prompt_tokens_details": {"cached_tokens": 900, "cache_write_tokens": 300}
        }
    });
    let server = MockInferenceServer::spawn(200, body).await.expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(move |r: &ResolvedProvider| openrouter::build(r, &base)),
    );
    let adapter = cfg
        .build("anthropic/claude-sonnet-4-5", &store)
        .expect("build");

    // Caller marks the system prefix as a cache breakpoint.
    let mut sys = ChatMessage::system("cache me");
    sys.cache_control = Some(CacheControl::ephemeral());
    let req = ChatRequest::new(
        "anthropic/claude-sonnet-4-5",
        vec![sys, ChatMessage::user("go")],
    );
    let resp = adapter.chat(&req).await.expect("chat ok");

    // Nested cache counters flowed into normalized Usage.
    let usage = resp.usage();
    assert_eq!(usage.cache_read_tokens, 900);
    assert_eq!(usage.cache_creation_tokens, 300);
    assert_eq!(usage.cost_usd, Some(0.0021));

    // cache_control passthrough: system content serialised as the block array.
    let sent = server.last_request().expect("captured");
    let body = sent.body.expect("json");
    assert_eq!(
        body["messages"][0]["content"],
        json!([{
            "type": "text",
            "text": "cache me",
            "cache_control": {"type": "ephemeral"}
        }])
    );
}

// ── Fireworks: no detailed-usage directive + tool-call round-trip ────────────────

/// Why: Fireworks must NOT inject the detailed-usage directive, must send tools +
/// the OpenAI-dialect `tool_choice`, and must parse a tool-call response
/// (round-trip through the full configurator cycle).
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn fireworks_tool_call_round_trip_no_usage_directive() {
    clear_provider_env();
    let body = json!({
        "id": "gen-fw",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"loc\":\"SEA\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 8}
    });
    let server = MockInferenceServer::spawn(200, body).await.expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    // Fireworks key present AND explicit fireworks/ prefix → resolves to Fireworks.
    store.set("fireworks", "fw-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::Fireworks,
        Box::new(move |r: &ResolvedProvider| fireworks::build(r, &base)),
    );

    let adapter = cfg
        .build("fireworks/llama-v3p1-8b-instruct", &store)
        .expect("build");
    assert_eq!(adapter.name(), "fireworks");

    let mut req = ChatRequest::new(
        "accounts/fireworks/models/llama-v3p1-8b-instruct",
        vec![ChatMessage::user("weather in SEA?")],
    );
    req.tools = Some(vec![ToolDefinition::function(FunctionDefinition {
        name: "get_weather".into(),
        description: Some("look up weather".into()),
        parameters: Some(json!({"type": "object"})),
        cache_control: None,
    })]);
    req.tool_choice = Some(adapter.map_tool_choice(ToolChoice::Auto));

    let resp = adapter.chat(&req).await.expect("chat ok");

    // Tool-call response parsing.
    let calls = resp.first_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "get_weather");

    // Request translation: tools + tool_choice present, NO usage directive.
    let sent = server.last_request().expect("captured");
    assert_eq!(sent.header("authorization"), Some("Bearer fw-fake")); // pragma: allowlist secret
    let body = sent.body.expect("json");
    assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(body["tool_choice"], "auto");
    assert!(
        body.get("usage").is_none() || body["usage"].is_null(),
        "Fireworks must not send the detailed-usage directive: {body}"
    );
}

// ── OpenAI-direct: bare OpenAI body via the shared core ──────────────────────────

/// Why: the OpenAI-direct adapter must send a bare OpenAI-compatible body (bearer
/// auth, no attribution headers, no usage directive) and parse the response.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn openai_direct_sends_bare_body() {
    clear_provider_env();
    let server = MockInferenceServer::spawn(200, text_response_body())
        .await
        .expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    store.set("openai", "sk-openai-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenAI,
        Box::new(move |r: &ResolvedProvider| openai::build(r, &base)),
    );

    // Explicit openai/ prefix + resolvable OPENAI key → OpenAI-direct.
    let adapter = cfg.build("openai/gpt-4o-mini", &store).expect("build");
    assert_eq!(adapter.name(), "openai");

    let req = ChatRequest::new("gpt-4o-mini", vec![ChatMessage::user("ping")]);
    let resp = adapter.chat(&req).await.expect("chat ok");
    assert_eq!(resp.first_text().as_deref(), Some("pong"));

    let sent = server.last_request().expect("captured");
    assert_eq!(sent.header("authorization"), Some("Bearer sk-openai-fake")); // pragma: allowlist secret
    assert!(sent.header("http-referer").is_none());
    let body = sent.body.expect("json");
    assert!(
        body.get("usage").is_none() || body["usage"].is_null(),
        "OpenAI-direct must not send the detailed-usage directive"
    );
}

// ── Together.ai: bare OpenAI body + tool-call round-trip via the shared core ─────

/// Why: driving Together through the `Configurator` must (a) send a bare
/// OpenAI-shaped body (bearer auth, no attribution headers, NO detailed-usage
/// directive), (b) carry tools + the OpenAI-dialect `tool_choice`, and (c) parse
/// a tool-call response — the full resolve→build→chat→usage cycle against a real
/// socket, exactly like the Fireworks e2e.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn together_tool_call_round_trip_no_usage_directive() {
    clear_provider_env();
    let body = json!({
        "id": "gen-together",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"loc\":\"SEA\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 8}
    });
    let server = MockInferenceServer::spawn(200, body).await.expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    // Together key present AND explicit together/ prefix → resolves to Together.
    store.set("together", "tgp_v1_fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::Together,
        Box::new(move |r: &ResolvedProvider| together::build(r, &base)),
    );

    let adapter = cfg
        .build("together/meta-llama/Llama-3.3-70B-Instruct-Turbo", &store)
        .expect("build");
    assert_eq!(adapter.name(), "together");
    assert_eq!(adapter.capabilities().id, ProviderId::Together);

    let mut req = ChatRequest::new(
        "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        vec![ChatMessage::user("weather in SEA?")],
    );
    req.tools = Some(vec![ToolDefinition::function(FunctionDefinition {
        name: "get_weather".into(),
        description: Some("look up weather".into()),
        parameters: Some(json!({"type": "object"})),
        cache_control: None,
    })]);
    req.tool_choice = Some(adapter.map_tool_choice(ToolChoice::Auto));

    let resp = adapter.chat(&req).await.expect("chat ok");

    // Tool-call response parsing.
    let calls = resp.first_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "get_weather");
    assert_eq!(resp.usage().total_tokens(), 28);

    // Request translation: bearer auth, no attribution, tools + tool_choice,
    // and NO detailed-usage directive.
    let sent = server.last_request().expect("captured");
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/chat/completions");
    assert_eq!(sent.header("authorization"), Some("Bearer tgp_v1_fake")); // pragma: allowlist secret
    assert!(sent.header("http-referer").is_none());
    let body = sent.body.expect("json");
    assert_eq!(body["model"], "meta-llama/Llama-3.3-70B-Instruct-Turbo");
    assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(body["tool_choice"], "auto");
    assert!(
        body.get("usage").is_none() || body["usage"].is_null(),
        "Together must not send the detailed-usage directive: {body}"
    );
}

// ── AtlasCloud: bare OpenAI body, no usage directive, nested-slug routing ────────

/// Why: driving AtlasCloud through the `Configurator` must (a) resolve the
/// `atlascloud/openai/gpt-5.6-sol` slug (whose model id is itself `vendor/model`
/// shaped) to the AtlasCloud adapter, (b) send a bare OpenAI-shaped body (bearer
/// auth, no attribution headers, NO detailed-usage directive), and (c) parse the
/// response text back — the full resolve→build→chat→usage cycle against a real
/// socket, exactly like the Together e2e.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn atlascloud_round_trip_no_usage_directive() {
    clear_provider_env();
    let server = MockInferenceServer::spawn(200, text_response_body())
        .await
        .expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    // AtlasCloud key present AND explicit atlascloud/ prefix → resolves to AtlasCloud.
    store.set("atlascloud", "ac_fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::AtlasCloud,
        Box::new(move |r: &ResolvedProvider| atlascloud::build(r, &base)),
    );

    let adapter = cfg
        .build("atlascloud/openai/gpt-5.6-sol", &store)
        .expect("build");
    assert_eq!(adapter.name(), "atlascloud");
    assert_eq!(adapter.capabilities().id, ProviderId::AtlasCloud);

    let req = ChatRequest::new("openai/gpt-5.6-sol", vec![ChatMessage::user("ping")]);
    let resp = adapter.chat(&req).await.expect("chat ok");
    assert_eq!(resp.first_text().as_deref(), Some("pong"));

    // Request translation: bearer auth, no attribution, NO detailed-usage directive.
    let sent = server.last_request().expect("captured");
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/chat/completions");
    assert_eq!(sent.header("authorization"), Some("Bearer ac_fake")); // pragma: allowlist secret
    assert!(sent.header("http-referer").is_none());
    let body = sent.body.expect("json");
    assert_eq!(body["model"], "openai/gpt-5.6-sol");
    assert!(
        body.get("usage").is_none() || body["usage"].is_null(),
        "AtlasCloud must not send the detailed-usage directive: {body}"
    );
}

// ── Anthropic-direct: native Messages-API wire format ────────────────────────────

/// A minimal successful Anthropic `/v1/messages` response body (text turn).
fn anthropic_text_response_body() -> Value {
    json!({
        "id": "msg_mock",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-5",
        "content": [{"type": "text", "text": "pong"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 12, "output_tokens": 3}
    })
}

/// Why: driving Anthropic-direct through the `Configurator` must (a) POST to the
/// `/messages` endpoint with the `x-api-key` + `anthropic-version` headers (NOT
/// an OpenAI `Authorization: Bearer`), (b) hoist `system` to the top-level param
/// and strip the `anthropic/` routing prefix from the model, and (c) parse the
/// native response text back — the full resolve→build→chat→usage cycle against a
/// real socket, exercising the bespoke (non-OpenAI) wire format.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn anthropic_direct_translates_messages_api_request() {
    clear_provider_env();
    let server = MockInferenceServer::spawn(200, anthropic_text_response_body())
        .await
        .expect("spawn mock");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    // Anthropic key present AND explicit anthropic/ prefix → resolves to Anthropic.
    store.set("anthropic", "sk-ant-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::Anthropic,
        Box::new(move |r: &ResolvedProvider| anthropic::build(r, &base)),
    );

    let adapter = cfg
        .build("anthropic/claude-sonnet-4-5", &store)
        .expect("build");
    assert_eq!(adapter.name(), "anthropic");
    assert_eq!(adapter.capabilities().id, ProviderId::Anthropic);

    let req = ChatRequest::new(
        "anthropic/claude-sonnet-4-5",
        vec![ChatMessage::system("be terse"), ChatMessage::user("ping")],
    );
    let resp = adapter.chat(&req).await.expect("chat ok");

    // Native response parsing.
    assert_eq!(resp.first_text().as_deref(), Some("pong"));
    assert_eq!(resp.usage().prompt_tokens, 12);
    assert_eq!(resp.usage().completion_tokens, 3);

    // Request translation captured by the mock.
    let sent = server.last_request().expect("captured request");
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/messages");
    assert_eq!(sent.header("x-api-key"), Some("sk-ant-fake")); // pragma: allowlist secret
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
    // Anthropic-direct authenticates via x-api-key, NOT an OpenAI bearer header.
    assert!(sent.header("authorization").is_none());
    let body = sent.body.expect("json body");
    // Model prefix stripped; system hoisted; max_tokens defaulted.
    assert_eq!(body["model"], "claude-sonnet-4-5");
    assert_eq!(body["system"], "be terse");
    assert!(body["max_tokens"].is_number());
    // The system turn is NOT in the messages array.
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["text"], "ping");
}

/// Why: the Anthropic-native usage payload (`input_tokens`/`output_tokens` plus
/// flat cache fields) must map into the normalized `Usage` (NOT deserialise to
/// zero through the OpenAI-shaped block — the #2403-review trap), a `tool_use`
/// content block must parse into an OpenAI-style tool call, and a caller-set
/// `cache_control` breakpoint must render on the wire as an ephemeral marker.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn anthropic_direct_parses_native_usage_and_tool_use() {
    clear_provider_env();
    let body = json!({
        "id": "msg_tool",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-5",
        "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
             "input": {"loc": "SEA"}}
        ],
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": 1200,
            "output_tokens": 40,
            "cache_read_input_tokens": 900,
            "cache_creation_input_tokens": 300
        }
    });
    let server = MockInferenceServer::spawn(200, body).await.expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    store.set("anthropic", "sk-ant-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::Anthropic,
        Box::new(move |r: &ResolvedProvider| anthropic::build(r, &base)),
    );
    let adapter = cfg
        .build("anthropic/claude-sonnet-4-5", &store)
        .expect("build");

    // Caller marks the system prefix as a cache breakpoint and supplies a tool.
    let mut sys = ChatMessage::system("cache me");
    sys.cache_control = Some(CacheControl::ephemeral());
    let mut req = ChatRequest::new(
        "anthropic/claude-sonnet-4-5",
        vec![sys, ChatMessage::user("weather?")],
    );
    req.tools = Some(vec![ToolDefinition::function(FunctionDefinition {
        name: "get_weather".into(),
        description: Some("look up weather".into()),
        parameters: Some(json!({"type": "object"})),
        cache_control: None,
    })]);
    req.tool_choice = Some(adapter.map_tool_choice(ToolChoice::Auto));

    let resp = adapter.chat(&req).await.expect("chat ok");

    // Native usage mapped correctly (input/output → prompt/completion, cache).
    let usage = resp.usage();
    assert_eq!(usage.prompt_tokens, 1200);
    assert_eq!(usage.completion_tokens, 40);
    assert_eq!(usage.cache_read_tokens, 900);
    assert_eq!(usage.cache_creation_tokens, 300);

    // tool_use block parsed into an OpenAI-style tool call.
    let calls = resp.first_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "get_weather");

    // Wire translation: system as a cache-annotated block array, Anthropic
    // tool shape, and the Anthropic tool_choice dialect.
    let sent = server.last_request().expect("captured");
    let body = sent.body.expect("json");
    assert_eq!(
        body["system"],
        json!([{
            "type": "text",
            "text": "cache me",
            "cache_control": {"type": "ephemeral"}
        }])
    );
    assert_eq!(body["tools"][0]["name"], "get_weather");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["tool_choice"], json!({"type": "auto"}));
}

// ── Error classification through a real socket ───────────────────────────────────

/// Why: a non-2xx provider status must surface as a classified `InferenceError`
/// — a 429 is retryable (not an alarm), driven end-to-end through the adapter.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn http_429_maps_to_retryable_api_error() {
    clear_provider_env();
    let server = MockInferenceServer::spawn(429, json!({"error": "rate limited"}))
        .await
        .expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(move |r: &ResolvedProvider| openrouter::build(r, &base)),
    );
    let adapter = cfg.build("x/y", &store).expect("build");

    let req = ChatRequest::new("x/y", vec![ChatMessage::user("hi")]);
    let Err(err) = adapter.chat(&req).await else {
        panic!("expected an API error");
    };
    assert!(matches!(err, InferenceError::Api { status: 429, .. }));
    assert!(err.is_retryable());
    assert!(!err.is_alarm());
}

// ── Default factory registration ─────────────────────────────────────────────────

/// Why: `register_default_factories` must wire the OpenAI-dialect providers
/// (OpenRouter, Fireworks, OpenAI, Together, AtlasCloud, Local) so a
/// bare/OpenRouter slug builds a real adapter through the public entry point,
/// an explicit `together/` slug (with a resolvable key) builds a real Together
/// adapter, and a `local/`/`ollama/` slug builds a real Local adapter with NO
/// stored credential required (Bedrock stays unregistered — a later wave).
#[test]
#[serial(dotenv_credential_env)]
fn default_factories_register_openai_dialect() {
    clear_provider_env();
    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").unwrap(); // pragma: allowlist secret
    store.set("together", "tgp_v1_fake").unwrap(); // pragma: allowlist secret
    store.set("atlascloud", "ac_fake").unwrap(); // pragma: allowlist secret

    let mut cfg = Configurator::new();
    register_default_factories(&mut cfg);

    // OpenRouter default path builds a real adapter.
    let adapter = cfg.build("some/model", &store).expect("build openrouter");
    assert_eq!(adapter.name(), "openrouter");

    // Together (#2488) is registered by the default set.
    let together = cfg
        .build("together/meta-llama/Llama-3.3-70B-Instruct-Turbo", &store)
        .expect("build together");
    assert_eq!(together.name(), "together");

    // AtlasCloud (#2536) is registered by the default set.
    let atlascloud = cfg
        .build("atlascloud/openai/gpt-5.6-sol", &store)
        .expect("build atlascloud");
    assert_eq!(atlascloud.name(), "atlascloud");
    assert_eq!(atlascloud.capabilities().id, ProviderId::AtlasCloud);

    // Local (#3247) is registered by the default set and needs no stored
    // credential at all — `local/`'s no-key resolution plus the factory's
    // placeholder-key fallback build a working adapter with an empty store.
    let local = cfg.build("local/llama3.1", &store).expect("build local");
    assert_eq!(local.name(), "local");
    assert_eq!(local.capabilities().id, ProviderId::Local);
    // The `ollama/` alias resolves to the same provider/factory.
    let ollama = cfg.build("ollama/qwen3:30b", &store).expect("build ollama");
    assert_eq!(ollama.name(), "local");

    // Bedrock is intentionally not registered by the default set.
    let Err(err) = cfg.build("bedrock/us.anthropic.claude-sonnet-4-5", &store) else {
        panic!("expected NoAdapterRegistered for Bedrock");
    };
    assert!(matches!(
        err,
        InferenceError::NoAdapterRegistered {
            provider: ProviderId::Bedrock
        }
    ));
}

/// Why: `register_default_factories` must also wire Anthropic-direct (#2408) so
/// an explicit `anthropic/` slug (with a resolvable `ANTHROPIC_API_KEY`) builds a
/// real native Anthropic adapter through the public entry point.
#[test]
#[serial(dotenv_credential_env)]
fn default_factories_register_anthropic_direct() {
    clear_provider_env();
    let store = MemoryKeyStore::new();
    store.set("anthropic", "sk-ant-fake").unwrap(); // pragma: allowlist secret

    let mut cfg = Configurator::new();
    register_default_factories(&mut cfg);

    let anthropic = cfg
        .build("anthropic/claude-sonnet-4-5", &store)
        .expect("build anthropic");
    assert_eq!(anthropic.name(), "anthropic");
    assert_eq!(anthropic.capabilities().id, ProviderId::Anthropic);
    // Native Anthropic dialect: `Required` → `{type:any}`, not OpenAI's "required".
    assert_eq!(
        anthropic.map_tool_choice(ToolChoice::Required),
        serde_json::json!({"type": "any"})
    );
}

// ── Streaming: real chat_stream over a hermetic SSE server (no live network) ──────

/// A canonical OpenRouter SSE completion: two content frames, a finish frame, a
/// usage-only frame (from `stream_options.include_usage`), then `[DONE]`.
const SSE_STREAM: &str = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";

/// A throwaway axum server that answers any POST with a fixed SSE body, streamed
/// as small byte chunks to mimic real socket framing (splitting frames — and
/// codepoints — across chunk boundaries). Shuts down when dropped.
struct SseServer {
    url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for SseServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

/// Spawn an [`SseServer`] serving `sse` as `text/event-stream`, chunked to 8-byte
/// pieces so the adapter's decoder is exercised across arbitrary splits.
async fn spawn_sse_server(sse: &'static str) -> SseServer {
    use axum::Router;
    use axum::body::Body;
    use axum::response::Response;

    async fn handler(axum::extract::State(sse): axum::extract::State<&'static str>) -> Response {
        let chunks: Vec<Result<Vec<u8>, std::convert::Infallible>> =
            sse.as_bytes().chunks(8).map(|c| Ok(c.to_vec())).collect();
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(futures_util::stream::iter(chunks)))
            .unwrap()
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    let app = Router::new().fallback(handler).with_state(sse);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });
    SseServer {
        url,
        shutdown: Some(tx),
        handle,
    }
}

/// Why: the real OpenAI-compat `chat_stream` transport must POST `stream:true`
/// and decode the provider's SSE frames into ordered text deltas plus a terminal
/// event carrying the finish reason + usage — end-to-end over a real socket
/// (reqwest `bytes_stream()` → `SseDecoder`), not just the parser in isolation.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn openrouter_chat_stream_yields_incremental_deltas() {
    use futures_util::StreamExt;
    use trusty_common::inference::{ChatStreamEvent, StopReason};

    clear_provider_env();
    let server = spawn_sse_server(SSE_STREAM).await;
    let base = server.url.clone();

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(move |r: &ResolvedProvider| openrouter::build(r, &base)),
    );
    let adapter = cfg.build("openai/gpt-4o-mini", &store).expect("build");

    let req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("ping")]);
    let stream = adapter
        .chat_stream(&req)
        .await
        .expect("stream handshake ok");
    let events: Vec<ChatStreamEvent> = stream.map(|r| r.expect("event ok")).collect().await;

    // Deltas concatenate in order to the full text.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ChatStreamEvent::Delta(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");

    // Exactly one terminal event, last, carrying usage + finish reason.
    match events.last().expect("terminal event") {
        ChatStreamEvent::Done(done) => {
            assert_eq!(done.usage.total_tokens(), 9);
            assert_eq!(done.finish_reason, Some(StopReason::Stop));
        }
        other => panic!("expected terminal Done, got {other:?}"),
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ChatStreamEvent::Done(_)))
            .count(),
        1,
        "exactly one terminal event"
    );
}

/// Why: a provider that rejects `stream=true` (non-2xx) must surface as the outer
/// `Err` from `chat_stream` — BEFORE any stream is returned — so the caller can
/// choose to retry non-streaming; the adapter must never silently degrade. Also
/// confirms the outbound body carried `stream:true`.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn chat_stream_surfaces_http_error_for_caller_retry() {
    clear_provider_env();
    let server = MockInferenceServer::spawn(400, json!({"error": {"message": "no stream"}}))
        .await
        .expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(move |r: &ResolvedProvider| openrouter::build(r, &base)),
    );
    let adapter = cfg.build("openai/gpt-4o-mini", &store).expect("build");

    let req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("ping")]);
    // `Ok(ChatStream)` is not `Debug`, so match rather than `expect_err`.
    match adapter.chat_stream(&req).await {
        Err(InferenceError::Api { status: 400, .. }) => {}
        Err(other) => panic!("expected retryable Api 400, got {other:?}"),
        Ok(_) => panic!("must surface an error, not a degraded stream"),
    }

    // The transport requested streaming on the wire.
    let sent = server.last_request().expect("captured request");
    assert_eq!(sent.body.expect("json body")["stream"], json!(true));
}

/// Why: a (mis)behaving gateway/LB can answer a `stream:true` request with 200
/// and a buffered, non-streaming JSON body (Content-Type `application/json`).
/// Feeding that to the SSE decoder would yield zero deltas + a clean `Done` and
/// silently drop the whole answer. `chat_stream` must detect the non-SSE body and
/// degrade gracefully — parse the buffered response and replay it — so the answer
/// still reaches the caller. (`MockInferenceServer` returns `application/json`.)
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn chat_stream_degrades_when_body_not_sse() {
    use futures_util::StreamExt;
    use trusty_common::inference::ChatStreamEvent;

    clear_provider_env();
    let server = MockInferenceServer::spawn(200, text_response_body())
        .await
        .expect("spawn");
    let base = server.url().to_string();

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-fake").unwrap(); // pragma: allowlist secret
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(move |r: &ResolvedProvider| openrouter::build(r, &base)),
    );
    let adapter = cfg.build("openai/gpt-4o-mini", &store).expect("build");

    let req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("ping")]);
    let stream = adapter
        .chat_stream(&req)
        .await
        .expect("non-SSE 200 must degrade to a buffered stream, not error");
    let events: Vec<ChatStreamEvent> = stream.map(|r| r.expect("event ok")).collect().await;

    // The full buffered answer is replayed, not dropped.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ChatStreamEvent::Delta(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "pong");
    match events.last().expect("terminal event") {
        ChatStreamEvent::Done(done) => assert_eq!(done.usage.total_tokens(), 8),
        other => panic!("expected terminal Done, got {other:?}"),
    }
}
