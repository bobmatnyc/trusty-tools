//! Integration coverage for the inference foundation (issue #2402, epic #2400).
//!
//! Why: the unit tests inside each submodule cover the pieces; this suite
//! exercises the PUBLIC surface exactly as a consumer will use it — the
//! capability registry queries, type serde round-trips, credential-resolution
//! wiring through an injected store, the two-stage `provider_for` resolver, and
//! the full `Configurator → ScriptedAdapter → chat → usage` cycle across the
//! `InferenceAdapter` trait object. This is where #2403's live-provider e2e will
//! attach: swap `ScriptedAdapter` for a real HTTP adapter pointed at
//! `test_support::MockInferenceServer` (see `mock_http_server_serves_response`).
//! What: black-box tests against `trusty_common::inference::*`.
//! Test: this file (run with `--features inference-client`).

use serial_test::serial;
use trusty_common::inference::credentials::{KeyStore, MemoryKeyStore};
use trusty_common::inference::test_support::ScriptedAdapter;
use trusty_common::inference::{
    ChatMessage, ChatRequest, ChatResponse, Configurator, InferenceAdapter, InferenceError,
    ProviderId, ResolvedProvider, ToolChoice, capabilities, capabilities_for, context_window,
    openai_tool_choice, pricing, provider_for,
};

/// Clear every provider env var so the store tier of the resolver is what tests
/// actually exercise (a stray `OPENROUTER_API_KEY` in the CI env would otherwise
/// shadow the injected `MemoryKeyStore`).
fn clear_provider_env() {
    for var in [
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "FIREWORKS_API_KEY",
    ] {
        // SAFETY: every caller is `#[serial(dotenv_credential_env)]`, matching
        // the lock the credential resolver's own env-mutating tests use.
        unsafe { std::env::remove_var(var) };
    }
}

// ── Capability registry ────────────────────────────────────────────────────────

/// Why: each seeded provider must be queryable by id and by name and expose a
/// coherent capability descriptor.
#[test]
fn registry_seeds_all_providers() {
    let expected = [
        (ProviderId::OpenRouter, "openrouter", true),
        (ProviderId::Fireworks, "fireworks", true),
        (ProviderId::Bedrock, "bedrock", false), // no API-key env (AWS chain)
        (ProviderId::Anthropic, "anthropic", true),
        (ProviderId::OpenAI, "openai", true),
        (ProviderId::Together, "together", true),
    ];
    for (id, name, has_key_env) in expected {
        let by_id = capabilities(id);
        let by_name = capabilities_for(name).expect("known provider by name");
        assert_eq!(by_id.id, id);
        assert_eq!(by_name.id, id);
        assert_eq!(by_id.credential_env.is_some(), has_key_env);
        assert!(by_id.native_tool_calling, "{name} should support tools");
    }
    assert!(capabilities_for("cohere").is_none());
}

/// Why: the #2330 fix must be reachable through the public `context_window`
/// query — haiku resolves to its real 200K window, not the 128K default.
#[test]
fn context_window_fixes_haiku_gap_2330() {
    assert_eq!(context_window("anthropic/claude-haiku-4-5", None), 200_000);
    assert_eq!(context_window("openai/gpt-4o-mini", None), 128_000);
    // Provider default tier: an unlisted model on Fireworks gets its 128K max.
    let fw = capabilities(ProviderId::Fireworks);
    assert_eq!(context_window("fireworks/new-model", Some(fw)), 128_000);
}

/// Why: pricing must resolve for the seeded families and be `None` for unknowns.
#[test]
fn pricing_resolves_known_families() {
    assert_eq!(
        pricing("anthropic/claude-sonnet-4-5").map(|p| p.input),
        Some(3.0)
    );
    assert_eq!(
        pricing("openai/gpt-5.4-nano-20260317").map(|p| p.input),
        Some(0.20)
    );
    assert!(pricing("cohere/command-r").is_none());
}

// ── Type round-trips ───────────────────────────────────────────────────────────

/// Why: the request/response wire types must round-trip through JSON so a
/// migrating consumer (#2406) can serialise a request and parse a response
/// without a shape change.
#[test]
fn request_and_response_round_trip() {
    let mut req = ChatRequest::new(
        "anthropic/claude-sonnet-4-5",
        vec![ChatMessage::system("be terse"), ChatMessage::user("hi")],
    );
    req.temperature = Some(0.2);
    req.tool_choice = Some(openai_tool_choice(ToolChoice::Auto));
    let wire = serde_json::to_string(&req).expect("serialise request");
    let back: ChatRequest = serde_json::from_str(&wire).expect("deserialise request");
    assert_eq!(back.model, "anthropic/claude-sonnet-4-5");
    assert_eq!(back.messages.len(), 2);
    assert_eq!(back.tool_choice, Some(serde_json::json!("auto")));

    let resp: ChatResponse = serde_json::from_str(
        r#"{"id":"g","model":"served/x",
             "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
             "usage":{"prompt_tokens":5,"completion_tokens":2,"cost":0.01}}"#,
    )
    .expect("deserialise response");
    assert_eq!(resp.first_text().as_deref(), Some("ok"));
    assert_eq!(resp.usage().cost_usd, Some(0.01));
    assert_eq!(resp.resolved_model("asked/x"), "served/x");
}

// ── Credential resolution + two-stage resolver ──────────────────────────────────

/// Why: `provider_for` must resolve a fake key through the injected store,
/// honour the explicit-prefix→family rule, and fall back to OpenRouter.
#[test]
#[serial(dotenv_credential_env)]
fn provider_for_two_stage_resolution() {
    clear_provider_env();

    // Explicit family whose key is present → that family.
    let store = MemoryKeyStore::new();
    store.set("anthropic", "sk-ant-key").unwrap(); // pragma: allowlist secret
    let r = provider_for("anthropic/claude-sonnet-4-5", &store).expect("resolves");
    assert_eq!(r.provider(), ProviderId::Anthropic);
    assert_eq!(r.key().map(|k| k.expose()), Some("sk-ant-key"));

    // Explicit family whose key is ABSENT → OpenRouter fallback.
    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-key").unwrap(); // pragma: allowlist secret
    let r = provider_for("anthropic/claude-sonnet-4-5", &store).expect("resolves");
    assert_eq!(r.provider(), ProviderId::OpenRouter);

    // Bedrock resolves with no key (AWS chain).
    let r = provider_for(
        "bedrock/us.anthropic.claude-sonnet-4-5",
        &MemoryKeyStore::new(),
    )
    .expect("resolves");
    assert_eq!(r.provider(), ProviderId::Bedrock);
    assert!(r.key().is_none());

    // Nothing anywhere → MissingCredential alarm.
    let err = provider_for("openai/gpt-4o-mini", &MemoryKeyStore::new()).expect_err("errors");
    assert!(err.is_alarm());
}

/// Why: a resolved key must never leak through `Debug` — the redacting
/// `SecretString` guards the whole `ResolvedProvider`.
#[test]
#[serial(dotenv_credential_env)]
fn resolved_provider_debug_is_redacted() {
    clear_provider_env();
    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-verysecret-abc").unwrap(); // pragma: allowlist secret
    let r = provider_for("x/y", &store).expect("resolves");
    let dumped = format!("{r:?}");
    assert!(
        !dumped.contains("verysecret"),
        "key leaked in Debug: {dumped}"
    );
}

// ── Configurator → adapter → chat → usage cycle ─────────────────────────────────

/// Why: the whole seam must work end-to-end — resolve a slug, build the
/// (scripted) adapter, drive it through the `InferenceAdapter` trait object, and
/// read back a normalized `Usage`. This is the cycle #2403's real adapters slot
/// into unchanged.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn configurator_builds_and_runs_scripted_adapter() {
    clear_provider_env();
    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-key").unwrap(); // pragma: allowlist secret

    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(|resolved: &ResolvedProvider| {
            let caps = capabilities(resolved.provider());
            Ok(Box::new(ScriptedAdapter::echo("openrouter", caps)) as Box<dyn InferenceAdapter>)
        }),
    );

    let adapter: Box<dyn InferenceAdapter> = cfg
        .build("openai/gpt-4o-mini", &store)
        .expect("built adapter");
    assert_eq!(adapter.name(), "openrouter");
    assert!(adapter.supports_native_tools());
    assert_eq!(
        adapter.context_window("anthropic/claude-haiku-4-5"),
        200_000
    );

    let req = ChatRequest::new(
        "openai/gpt-4o-mini",
        vec![ChatMessage::user("round trip please")],
    );
    let resp = adapter.chat(&req).await.expect("chat ok");
    assert_eq!(resp.first_text().as_deref(), Some("round trip please"));
    assert!(resp.usage().total_tokens() > 0);
}

/// Why: building against a provider with no registered factory must be an
/// explicit alarm, never a silent fallback.
#[test]
#[serial(dotenv_credential_env)]
fn configurator_unregistered_provider_alarms() {
    clear_provider_env();
    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-key").unwrap(); // pragma: allowlist secret
    let cfg = Configurator::new();
    // `Box<dyn InferenceAdapter>` is not `Debug`; match the error out.
    let Err(err) = cfg.build("x/y", &store) else {
        panic!("expected NoAdapterRegistered");
    };
    assert!(matches!(err, InferenceError::NoAdapterRegistered { .. }));
    assert!(err.is_alarm());
}

// ── axum HTTP mock (where #2403's live-provider e2e attaches) ────────────────────

/// Why: the future concrete adapters need a real socket to test against; this
/// proves the `axum-server`-gated mock serves the canned chat/completions body
/// #2403 will parse. Only compiled when `axum-server` is enabled (e.g.
/// `--all-features`).
#[cfg(feature = "axum-server")]
#[tokio::test]
async fn mock_http_server_serves_response() {
    use trusty_common::inference::test_support::MockInferenceServer;

    let body = serde_json::json!({
        "id": "gen-mock",
        "choices": [{"message": {"role": "assistant", "content": "from mock"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2}
    });
    let server = MockInferenceServer::spawn(200, body)
        .await
        .expect("spawn mock");
    let raw = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", server.url()))
        .send()
        .await
        .expect("send")
        .text()
        .await
        .expect("body");
    // The body a real adapter would deserialise straight into ChatResponse.
    let resp: ChatResponse = serde_json::from_str(&raw).expect("parse as ChatResponse");
    assert_eq!(resp.first_text().as_deref(), Some("from mock"));
}
