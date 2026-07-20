//! Unit + integration-style tests for `llm::http` (moved out of `mod.rs`
//! per #2410 to keep the production file under the 500-SLOC cap after
//! adding Fireworks credential-resolution coverage).
//!
//! Why: `check_line_cap.sh` classifies `mod.rs`/plain `.rs` files as
//! production source (500-SLOC cap) regardless of an inline `#[cfg(test)]`
//! block's size; this crate's established pattern for a test module that
//! outgrows that budget is a sibling `tests.rs` (see `llm/adapter/`,
//! `llm/inference_client/`).
//! What: Every test that previously lived inline at the bottom of
//! `llm/http.rs`, unchanged.
//! Test: This module IS the test.

use super::*;
use async_openai::types::{
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
};

/// Why (fix regression test): `send_raw_completion` takes its auth value
/// from `adapter.api_endpoint(false)`, which for `GenericAdapter` (no
/// override) is `openrouter_endpoint()` — the exact function that
/// previously read `OPENROUTER_API_KEY` via a raw `std::env::var`, so a
/// credential configured ONLY via the secure store never reached the
/// request. This test proves the credential now resolves through the
/// shared 3-tier resolver (env > `.env.local` > secure store) by seeding
/// a `FileKeyStore` directly, with env absent, and asserting the request
/// actually reaches the network (a loopback, connection-refused target)
/// with a non-empty bearer token instead of bailing with "credential not
/// found".
/// Test: itself.
// NOTE: none of the three async tests below hold `crate::test_env::{ENV_LOCK,
// HOME_LOCK}` (`std::sync::Mutex`) across their `.await` — clippy's
// `await_holding_lock` correctly forbids that for a sync mutex. `#[serial]`
// (unnamed group) provides the cross-test exclusion instead, matching the
// established pattern in `inference_client::tests::with_store_honours_openrouter_base_url_override`.

#[tokio::test]
#[serial_test::serial]
async fn send_raw_completion_resolves_key_from_store_when_env_absent() {
    let prev_openrouter = std::env::var_os("OPENROUTER_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: `#[serial]` (unnamed group) serializes against every other
    // unnamed `#[serial]` test in this binary.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }

    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let store = trusty_common::inference::credentials::FileKeyStore::at(tmp.path());
    trusty_common::inference::credentials::KeyStore::set(
        &store,
        "openrouter",
        "sk-or-FAKE-store-value", // pragma: allowlist secret
    )
    .expect("seed store");

    // GenericAdapter's default `api_endpoint` is `openrouter_endpoint()`
    // pointed at the real OpenRouter host but with NO override — routing
    // to a real host with a fake key would attempt a live network call.
    // Point it at an unroutable loopback port instead so the request
    // fails fast on connection refusal (not on auth), letting us assert
    // the fallback resolved a non-empty key without touching the network.
    // SAFETY: still under `#[serial]` exclusion.
    unsafe {
        std::env::set_var("OPENROUTER_BASE_URL", "http://127.0.0.1:1");
    }

    let adapter = crate::llm::adapter::GenericAdapter;
    let body = serde_json::json!({"model": "gpt-4o", "messages": []});
    let err = send_raw_completion(&body, &adapter)
        .await
        .expect_err("connection to 127.0.0.1:1 must fail");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("credential not found") && !msg.contains("not set"),
        "must not report a missing credential when the store has one: {msg}"
    );

    // SAFETY: still under `#[serial]` exclusion.
    unsafe {
        std::env::remove_var("OPENROUTER_BASE_URL");
        match prev_openrouter {
            Some(v) => std::env::set_var("OPENROUTER_API_KEY", v),
            None => std::env::remove_var("OPENROUTER_API_KEY"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Why: when NO tier (env, `.env.local`, or store) resolves a credential,
/// `send_raw_completion` must fail with a clear, provider-named error
/// instead of silently sending an empty bearer token and letting the
/// provider's bare 401 stand in for the real problem.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn send_raw_completion_missing_everywhere_errors_with_provider_name() {
    let prev_openrouter = std::env::var_os("OPENROUTER_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: `#[serial]` (unnamed group) provides exclusion.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    // No store seeded — every tier is absent.

    let adapter = crate::llm::adapter::GenericAdapter;
    let body = serde_json::json!({"model": "gpt-4o", "messages": []});
    let err = send_raw_completion(&body, &adapter)
        .await
        .expect_err("no credential anywhere must error, not send an empty key");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("openrouter") && msg.contains("credential not found"),
        "error must name the provider and say credential not found: {msg}"
    );

    // SAFETY: still under `#[serial]` exclusion.
    unsafe {
        match prev_openrouter {
            Some(v) => std::env::set_var("OPENROUTER_API_KEY", v),
            None => std::env::remove_var("OPENROUTER_API_KEY"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Adapter fixture whose `ApiEndpoint` supplies NO credential (empty
/// `auth_header_value`, mirroring `OllamaAdapter`'s "no auth needed"
/// shape) so `send_raw_completion`'s literal fallback branch — `if
/// endpoint.auth_header_value.is_empty() { resolve_key("openrouter")... }`
/// — is exercised directly rather than via a populated `ApiEndpoint`.
#[derive(Debug)]
struct NoCredentialAdapter;

impl ModelAdapter for NoCredentialAdapter {
    fn provider(&self) -> crate::llm::adapter::Provider {
        crate::llm::adapter::Provider::Generic
    }
    fn tool_choice_any(&self) -> Option<serde_json::Value> {
        None
    }
    fn tool_choice_auto(&self) -> Option<serde_json::Value> {
        None
    }
    fn inject_cache_control(&self, _: &mut serde_json::Value, _: bool) {}
    fn parse_usage(&self, _: &serde_json::Value) -> TokenUsage {
        TokenUsage::default()
    }
    fn api_endpoint(&self, _use_direct: bool) -> adapter::ApiEndpoint {
        adapter::ApiEndpoint {
            base_url: "http://127.0.0.1:1".to_string(),
            auth_header_name: "Authorization".to_string(),
            auth_header_value: String::new(),
            extra_headers: vec![],
            auth_source: adapter::AuthSource::OpenRouter,
        }
    }
}

/// Why: the literal `send_raw_completion` fallback branch (an adapter
/// that supplies NO credential in its `ApiEndpoint`) must still resolve
/// through the shared 3-tier resolver rather than a raw env read.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn send_raw_completion_empty_endpoint_credential_falls_back_to_store() {
    let prev_openrouter = std::env::var_os("OPENROUTER_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: `#[serial]` (unnamed group) provides exclusion.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    let store = trusty_common::inference::credentials::FileKeyStore::at(tmp.path());
    trusty_common::inference::credentials::KeyStore::set(
        &store,
        "openrouter",
        "sk-or-FAKE-store-value", // pragma: allowlist secret
    )
    .expect("seed store");

    let adapter = NoCredentialAdapter;
    let body = serde_json::json!({"model": "gpt-4o", "messages": []});
    let err = send_raw_completion(&body, &adapter)
        .await
        .expect_err("connection to 127.0.0.1:1 must fail");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("credential not found"),
        "must not report a missing credential when the store has one: {msg}"
    );

    // SAFETY: still under `#[serial]` exclusion.
    unsafe {
        match prev_openrouter {
            Some(v) => std::env::set_var("OPENROUTER_API_KEY", v),
            None => std::env::remove_var("OPENROUTER_API_KEY"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Why: (#2410) before this fix, a Fireworks-routed call with no
/// `FIREWORKS_API_KEY` configured anywhere reported "openrouter
/// credential not found" — the wrong provider name AND the wrong env
/// var/config hint. This proves the error now names Fireworks.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn send_raw_completion_fireworks_missing_key_errors_with_fireworks_name() {
    let prev_fireworks = std::env::var_os("FIREWORKS_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: `#[serial]` (unnamed group) provides exclusion.
    unsafe {
        std::env::remove_var("FIREWORKS_API_KEY");
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    // No store seeded — every tier is absent.

    let adapter = crate::llm::adapter::FireworksAdapter {
        model_id: "accounts/fireworks/models/llama-v3p1-8b-instruct".to_string(),
    };
    let body = serde_json::json!({"model": "accounts/fireworks/models/llama-v3p1-8b-instruct", "messages": []});
    let err = send_raw_completion(&body, &adapter)
        .await
        .expect_err("no fireworks credential anywhere must error, not send an empty key");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("fireworks") && msg.contains("credential not found"),
        "error must name fireworks, not openrouter: {msg}"
    );
    assert!(
        msg.contains("FIREWORKS_API_KEY"),
        "error must hint the correct env var: {msg}"
    );

    // SAFETY: still under `#[serial]` exclusion.
    unsafe {
        match prev_fireworks {
            Some(v) => std::env::set_var("FIREWORKS_API_KEY", v),
            None => std::env::remove_var("FIREWORKS_API_KEY"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Why: (#2410) `send_raw_completion` must resolve a Fireworks credential
/// configured ONLY in the secure store (env absent) — mirrors
/// `send_raw_completion_resolves_key_from_store_when_env_absent` but for
/// the new `FireworksAdapter` instead of `GenericAdapter`/OpenRouter.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn send_raw_completion_fireworks_resolves_key_from_store_when_env_absent() {
    let prev_fireworks = std::env::var_os("FIREWORKS_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: `#[serial]` (unnamed group) serializes against every other
    // unnamed `#[serial]` test in this binary.
    unsafe {
        std::env::remove_var("FIREWORKS_API_KEY");
    }

    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let store = trusty_common::inference::credentials::FileKeyStore::at(tmp.path());
    trusty_common::inference::credentials::KeyStore::set(
        &store,
        "fireworks",
        "fw-FAKE-store-value", // pragma: allowlist secret
    )
    .expect("seed store");

    // Point the adapter at an unroutable loopback port so the request
    // fails fast on connection refusal (not on auth), proving the
    // fallback resolved a non-empty key without touching the network.
    // SAFETY: still under `#[serial]` exclusion.
    unsafe {
        std::env::set_var("FIREWORKS_BASE_URL", "http://127.0.0.1:1/inference/v1");
    }

    let adapter = crate::llm::adapter::FireworksAdapter {
        model_id: "accounts/fireworks/models/llama-v3p1-8b-instruct".to_string(),
    };
    let body = serde_json::json!({"model": "accounts/fireworks/models/llama-v3p1-8b-instruct", "messages": []});
    let err = send_raw_completion(&body, &adapter)
        .await
        .expect_err("connection to 127.0.0.1:1 must fail");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("credential not found"),
        "must not report a missing credential when the store has one: {msg}"
    );

    // SAFETY: still under `#[serial]` exclusion.
    unsafe {
        std::env::remove_var("FIREWORKS_BASE_URL");
        match prev_fireworks {
            Some(v) => std::env::set_var("FIREWORKS_API_KEY", v),
            None => std::env::remove_var("FIREWORKS_API_KEY"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Live smoke test: send a trivial prompt through the REAL trusty-agents
/// stack (`FireworksAdapter::api_endpoint` credential resolution +
/// `send_raw_completion`'s HTTP POST) to the real `api.fireworks.ai`.
///
/// Why: (#2410, epic #2400, Step 3) proves Fireworks is reachable
/// end-to-end through THIS crate's dispatch path — not just through
/// `trusty_common::inference`'s own adapter (already covered by that
/// crate's `live_fireworks_call`). Ignored so CI stays offline; run locally
/// with a real `FIREWORKS_API_KEY` (env, `.env.local`, or the secure store —
/// this test resolves through the same 3-tier resolver production code
/// uses, so a store-only key is sufficient).
/// What: Builds a `FireworksAdapter`, a raw OpenAI-compatible chat body
/// (mirroring what `tool_loop::mod`'s raw path sends for a
/// `fireworks/*`-routed turn), and calls `send_raw_completion` directly.
/// Skips (does not fail) when no Fireworks credential resolves anywhere.
/// Test: `cargo test -p trusty-agents --test-threads=1 \
///        live_fireworks_call_through_agent_adapter -- --ignored --nocapture`
///        (with `FIREWORKS_API_KEY` set, or `tagent config keys set fireworks`).
#[tokio::test]
#[ignore = "requires a Fireworks credential; skipped in CI"]
async fn live_fireworks_call_through_agent_adapter() {
    if trusty_common::inference::credentials::resolve_key("fireworks").is_none() {
        eprintln!("no fireworks credential resolves anywhere — skipping live test");
        return;
    }

    // A model current as of writing (`fireworks/models` catalog query);
    // Fireworks periodically retires serverless models, so this may need
    // updating — a 404 "Model not found" here means the model, not the
    // adapter/credential path, is stale.
    let model_id = "accounts/fireworks/models/gpt-oss-120b";
    let adapter = crate::llm::adapter::FireworksAdapter {
        model_id: model_id.to_string(),
    };
    let body = serde_json::json!({
        "model": model_id,
        "messages": [
            {"role": "system", "content": "You are a concise assistant."},
            {"role": "user", "content": "Reply with exactly the word: pong"}
        ],
        "temperature": 0.0,
        "max_tokens": 200,
    });

    let (content, _tool_calls, usage) = send_raw_completion(&body, &adapter)
        .await
        .expect("live fireworks chat via trusty-agents dispatch path");
    let text = content.expect("assistant text");
    assert!(!text.is_empty(), "assistant text was empty");
    assert!(usage.prompt_tokens > 0, "prompt_tokens should be > 0");
    eprintln!("live fireworks (trusty-agents adapter) ok — text: {text:?}, usage: {usage:?}");
}

#[test]
fn http_client_returns_same_instance() {
    // MIN-2 (#98): the module-level OnceLock must hand out the same
    // reqwest::Client across calls so connection pooling actually kicks
    // in. Pointer equality is the simplest way to assert identity.
    let a = http_client();
    let b = http_client();
    assert!(std::ptr::eq(a, b));
}

#[test]
fn strip_service_tier_removes_field() {
    // #486: OpenRouter returns `service_tier` values async-openai's
    // `ServiceTier` enum can't deserialize (e.g. "standard"). The helper
    // must drop the field so the rest of the response deserializes.
    let json = serde_json::json!({
        "id": "chatcmpl-1",
        "service_tier": "standard",
        "choices": [],
    });
    let cleaned = strip_service_tier(json);
    assert!(cleaned.get("service_tier").is_none());
    assert_eq!(
        cleaned.get("id").and_then(|v| v.as_str()),
        Some("chatcmpl-1")
    );
    assert!(cleaned.get("choices").is_some());

    // Non-object values pass through untouched.
    let arr = serde_json::json!([1, 2, 3]);
    assert_eq!(strip_service_tier(arr.clone()), arr);
}

#[test]
fn build_raw_request_injects_cache_control() {
    let system: ChatCompletionRequestMessage = ChatCompletionRequestSystemMessageArgs::default()
        .content("You are a helpful assistant.")
        .build()
        .unwrap()
        .into();
    let user: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
        .content("hello")
        .build()
        .unwrap()
        .into();
    let body =
        build_raw_request("claude-sonnet-4-5", &[system, user], &[], 0.2, 1024, true).unwrap();
    let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
    let sys = &messages[0];
    assert_eq!(sys["role"], "system");
    let content = sys["content"].as_array().expect("content is array");
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "You are a helpful assistant.");
    assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn build_raw_request_without_cache_control_leaves_system_alone() {
    let system: ChatCompletionRequestMessage = ChatCompletionRequestSystemMessageArgs::default()
        .content("sys")
        .build()
        .unwrap()
        .into();
    let body = build_raw_request("gpt-4", &[system], &[], 0.1, 100, false).unwrap();
    let msgs = body["messages"].as_array().unwrap();
    let sys_content = &msgs[0]["content"];
    if let Some(arr) = sys_content.as_array() {
        for block in arr {
            assert!(block.get("cache_control").is_none());
        }
    }
}
