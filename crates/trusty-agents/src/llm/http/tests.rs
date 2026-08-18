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
//! `llm/http.rs`, plus the new Fireworks credential-resolution coverage.
//! Behaviour is unchanged from the inline versions; the only edit made
//! during the move was carrying over #3464's `force_env_local_loaded()`
//! preamble, which landed on `main` after this split was authored.
//! #3952 later converted the five `$HOME`-sandboxing credential tests from
//! `#[tokio::test]` to sync `#[test]` + [`block_on`] so they can join the
//! crate's single `$HOME` lock domain — assertions unchanged.
//! Test: This module IS the test.

use super::*;
use async_openai::types::{
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
};

/// Drive `fut` to completion on a private CURRENT-THREAD tokio runtime.
///
/// Why (#3952): the five credential-resolution tests below sandbox `$HOME`,
/// so they must hold `crate::test_env::lock_home()` — the crate's single
/// `$HOME` synchronization domain — for their whole body, including the
/// awaited `send_raw_completion` call. As `#[tokio::test]`s they could not:
/// `HOME_LOCK` is a `std::sync::Mutex` and clippy's `await_holding_lock`
/// correctly forbids holding its guard across `.await`, which is precisely
/// why they were left on `#[serial]` alone and became the landmine #3952
/// describes. Inverting the structure removes the conflict instead of
/// suppressing it: a SYNC `#[test]` holds the guards and hands the future to
/// an explicit runtime, so there is no `.await` in the guarded scope at all
/// and the lint never applies. This is the same manoeuvre PR #3976 used to
/// bring the embedder reference-accuracy test under its module's existing
/// std lock (issue #3711).
/// What: builds a fresh `Builder::new_current_thread().enable_all()` runtime
/// — current-thread, never `Runtime::new()`/`new_multi_thread()`, both of
/// which `crate::test_env::lock_home` now rejects outright (#3957) — and
/// `block_on`s the future on the calling thread, which is the same thread
/// that owns the `HomeLockGuard`.
/// Test: used by every `send_raw_completion_*` test in this module.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
        .block_on(fut)
}

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
// #3952: every test below that sandboxes `$HOME` now holds
// `crate::test_env::lock_home()` for its whole body — the crate's ONE `$HOME`
// synchronization domain — instead of relying on `#[serial]` alone.
//
// The old arrangement was two mutually unaware exclusion mechanisms:
// `#[serial]` (unnamed group) only excludes other `#[serial]`-tagged tests,
// while dozens of `$HOME`-sandboxing tests elsewhere in this crate
// (`listeners::poll`, `tools::mcp_tools`, `init::tests::*`, `api::server`,
// `slack`, ...) exclude each other via `HOME_LOCK` and carry no `#[serial]`.
// Neither mechanism serializes against the other, so this file's `$HOME`
// swaps raced all of them — confirmed as the root cause of #3922's
// `listeners::store::tests::dedup_seed_loads_recent_ids` CI flake. Two locks
// do not serialize against each other; that is the same defect shape PR
// #3976 fixed in the embedder module by collapsing two env-lock statics into
// one, and the fix here is the same discipline, not a third mechanism.
//
// `#[serial]` is RETAINED (not replaced) because these tests also mutate
// process-global credential env vars, whose domain is `ENV_LOCK` + the
// crate-wide `#[serial]` convention documented in `llm::helpers::tests` —
// unrelated to `$HOME`. Lock order matches every other multi-lock test in
// this crate (`llm::{credentials,helpers,adapter}::tests`,
// `system_status::credentials`): `#[serial]` → `ENV_LOCK` → `HOME_LOCK`.
//
// Holding sync guards is possible at all because these are now sync
// `#[test]`s driving `block_on` (see this module's helper), so clippy's
// `await_holding_lock` — the reason they were on `#[serial]` alone — no
// longer applies to anything.

#[test]
#[serial_test::serial]
fn send_raw_completion_resolves_key_from_store_when_env_absent() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // #3952: single `$HOME` domain — `lock_home()`, not the raw `HOME_LOCK`,
    // so `listeners::store::events_dir`'s per-thread ownership guard also
    // recognises this test as participating.
    let _home_guard = crate::test_env::lock_home();
    // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
    crate::test_env::force_env_local_loaded();
    let prev_openrouter = std::env::var_os("OPENROUTER_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: ENV_LOCK + HOME_LOCK held for the whole test body.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }

    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let store = trusty_common::credentials::FileKeyStore::at(tmp.path());
    trusty_common::credentials::KeyStore::set(
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
    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
    unsafe {
        std::env::set_var("OPENROUTER_BASE_URL", "http://127.0.0.1:1");
    }

    let adapter = crate::llm::adapter::GenericAdapter;
    let body = serde_json::json!({"model": "gpt-4o", "messages": []});
    let err = block_on(send_raw_completion(&body, &adapter))
        .expect_err("connection to 127.0.0.1:1 must fail");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("credential not found") && !msg.contains("not set"),
        "must not report a missing credential when the store has one: {msg}"
    );

    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
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
#[test]
#[serial_test::serial]
fn send_raw_completion_missing_everywhere_errors_with_provider_name() {
    // #3952: ENV_LOCK then HOME_LOCK, held for the whole body.
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _home_guard = crate::test_env::lock_home();
    // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
    crate::test_env::force_env_local_loaded();
    let prev_openrouter = std::env::var_os("OPENROUTER_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: ENV_LOCK + HOME_LOCK held for the whole test body.
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
    let err = block_on(send_raw_completion(&body, &adapter))
        .expect_err("no credential anywhere must error, not send an empty key");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("openrouter") && msg.contains("credential not found"),
        "error must name the provider and say credential not found: {msg}"
    );

    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
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
#[test]
#[serial_test::serial]
fn send_raw_completion_empty_endpoint_credential_falls_back_to_store() {
    // #3952: ENV_LOCK then HOME_LOCK, held for the whole body.
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _home_guard = crate::test_env::lock_home();
    // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
    crate::test_env::force_env_local_loaded();
    let prev_openrouter = std::env::var_os("OPENROUTER_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: ENV_LOCK + HOME_LOCK held for the whole test body.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    let store = trusty_common::credentials::FileKeyStore::at(tmp.path());
    trusty_common::credentials::KeyStore::set(
        &store,
        "openrouter",
        "sk-or-FAKE-store-value", // pragma: allowlist secret
    )
    .expect("seed store");

    let adapter = NoCredentialAdapter;
    let body = serde_json::json!({"model": "gpt-4o", "messages": []});
    let err = block_on(send_raw_completion(&body, &adapter))
        .expect_err("connection to 127.0.0.1:1 must fail");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("credential not found"),
        "must not report a missing credential when the store has one: {msg}"
    );

    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
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
#[test]
#[serial_test::serial]
fn send_raw_completion_fireworks_missing_key_errors_with_fireworks_name() {
    // #3952: ENV_LOCK then HOME_LOCK, held for the whole body.
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _home_guard = crate::test_env::lock_home();
    // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
    crate::test_env::force_env_local_loaded();
    let prev_fireworks = std::env::var_os("FIREWORKS_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: ENV_LOCK + HOME_LOCK held for the whole test body.
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
    let err = block_on(send_raw_completion(&body, &adapter))
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

    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
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
#[test]
#[serial_test::serial]
fn send_raw_completion_fireworks_resolves_key_from_store_when_env_absent() {
    // #3952: ENV_LOCK then HOME_LOCK, held for the whole body.
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _home_guard = crate::test_env::lock_home();
    // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
    crate::test_env::force_env_local_loaded();
    let prev_fireworks = std::env::var_os("FIREWORKS_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: ENV_LOCK + HOME_LOCK held for the whole test body.
    unsafe {
        std::env::remove_var("FIREWORKS_API_KEY");
    }

    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let store = trusty_common::credentials::FileKeyStore::at(tmp.path());
    trusty_common::credentials::KeyStore::set(
        &store,
        "fireworks",
        "fw-FAKE-store-value", // pragma: allowlist secret
    )
    .expect("seed store");

    // Point the adapter at an unroutable loopback port so the request
    // fails fast on connection refusal (not on auth), proving the
    // fallback resolved a non-empty key without touching the network.
    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
    unsafe {
        std::env::set_var("FIREWORKS_BASE_URL", "http://127.0.0.1:1/inference/v1");
    }

    let adapter = crate::llm::adapter::FireworksAdapter {
        model_id: "accounts/fireworks/models/llama-v3p1-8b-instruct".to_string(),
    };
    let body = serde_json::json!({"model": "accounts/fireworks/models/llama-v3p1-8b-instruct", "messages": []});
    let err = block_on(send_raw_completion(&body, &adapter))
        .expect_err("connection to 127.0.0.1:1 must fail");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("credential not found"),
        "must not report a missing credential when the store has one: {msg}"
    );

    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
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
    if trusty_common::credentials::resolve_key("fireworks").is_none() {
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

// --- #3766: transport-failure classification -------------------------------

/// A connect failure (nothing listening on the port) is a TRANSPORT error:
/// no HTTP status was ever received. This is the owner's exact shape —
/// ollama not running on localhost:11434.
#[tokio::test]
async fn transport_error_detects_connect_failure() {
    // Port 1 on loopback: reserved, never listening — a deterministic
    // connect-refused without touching the network.
    let err = reqwest::Client::new()
        .get("http://127.0.0.1:1/v1/chat/completions")
        .send()
        .await
        .expect_err("connect to a closed port must fail");
    let wrapped = anyhow::Error::new(err).context("raw chat completion POST failed");
    assert!(
        super::is_transport_error(&wrapped),
        "connect failure must classify as transport: {wrapped:#}"
    );
}

/// An error with no `reqwest::Error` in its chain is not a transport failure —
/// the classifier must not treat arbitrary errors as retryable.
#[test]
fn transport_error_rejects_non_http_error() {
    let err = anyhow::anyhow!("model returned malformed tool call");
    assert!(!super::is_transport_error(&err));
}

// ── AtlasCloud (#3765) ───────────────────────────────────────────────────────

/// Why: the AtlasCloud counterpart of
/// `send_raw_completion_fireworks_missing_key_errors_with_fireworks_name`. A
/// keyless AtlasCloud call previously reported "openrouter credential not
/// found" — the wrong provider, the wrong env var, and the wrong config
/// command — because `credential_hint` had no arm for it.
/// Test: itself.
#[test]
fn send_raw_completion_atlascloud_missing_key_errors_with_atlascloud_name() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _home_guard = crate::test_env::lock_home();
    crate::test_env::force_env_local_loaded();
    let prev_key = std::env::var_os("ATLASCLOUD_API_KEY");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: ENV_LOCK + HOME_LOCK held for the whole test body.
    unsafe {
        std::env::remove_var("ATLASCLOUD_API_KEY");
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    // No store seeded — every tier is absent.

    let adapter = crate::llm::adapter::AtlasCloudAdapter {
        model_id: "openai/gpt-5.6-sol".to_string(),
    };
    let body = serde_json::json!({"model": "openai/gpt-5.6-sol", "messages": []});
    let err = block_on(send_raw_completion(&body, &adapter))
        .expect_err("no atlascloud credential anywhere must error, not send an empty key");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("atlascloud") && msg.contains("credential not found"),
        "error must name atlascloud, not openrouter: {msg}"
    );
    assert!(
        msg.contains("ATLASCLOUD_API_KEY"),
        "error must hint the correct env var: {msg}"
    );

    // SAFETY: still holding ENV_LOCK + HOME_LOCK.
    unsafe {
        match prev_key {
            Some(v) => std::env::set_var("ATLASCLOUD_API_KEY", v),
            None => std::env::remove_var("ATLASCLOUD_API_KEY"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Live smoke test: one real completion through the AtlasCloud adapter.
///
/// Why: every other AtlasCloud assertion in this PR is offline — they prove
/// the routing, not that `api.atlascloud.ai` actually accepts the body
/// `send_raw_completion` builds. The OpenAI-compatible shape is INFERRED from
/// the registry seed and `providers::atlascloud`; this is what turns that
/// inference into an observation.
/// What: resolves the credential through the normal 3-tier resolver (never a
/// hardcoded key) and SKIPS — does not fail — when it is absent, so CI, which
/// has no AtlasCloud key, stays green. `#[ignore]` for the same reason the
/// ONNX embedder tests are ignored: it needs an environment CI does not have.
///
/// The model comes from `ATLASCLOUD_TEST_MODEL` when set, else the registry
/// seed default. The override exists because AtlasCloud scopes its catalog by
/// ACCOUNT PLAN, not just by key validity: a Coding-Plan key answers
/// `403 {"msg":"invalid token for coding plan, this model not support coding
/// plan"}` for catalog ids it can see but not call, including the seeded
/// default. That is a property of the account, not of this adapter, so the
/// test lets the operator name a model their key can actually reach rather
/// than pinning the registry seed to one plan's subset.
/// Run it with:
/// `cargo test -p trusty-agents --lib atlascloud_live -- --ignored --nocapture`
/// Test: itself.
#[test]
#[ignore = "requires ATLASCLOUD_API_KEY; skipped in CI"]
fn atlascloud_live_completion_round_trips() {
    crate::test_env::force_env_local_loaded();
    if trusty_common::credentials::resolve_key("atlascloud").is_none() {
        eprintln!("ATLASCLOUD_API_KEY not resolvable — skipping live test");
        return;
    }
    let model = std::env::var("ATLASCLOUD_TEST_MODEL").unwrap_or_else(|_| {
        trusty_common::inference::registry::capabilities_for("atlascloud")
            .expect("seeded")
            .default_model
            .to_string()
    });
    let slug = format!("atlascloud/{model}");
    let adapter = crate::llm::adapter::adapter_for_model(&slug);
    assert_eq!(
        adapter.provider(),
        crate::llm::adapter::Provider::AtlasCloud
    );
    assert_eq!(adapter.wire_model_id(&slug), model);
    let body = serde_json::json!({
        "model": adapter.wire_model_id(&slug),
        "messages": [
            {"role": "system", "content": "You are a concise assistant."},
            {"role": "user", "content": "Reply with exactly the word: pong"}
        ],
        "max_tokens": 256,
        "temperature": 0.0,
    });
    let (text, _tool_calls, usage) =
        block_on(send_raw_completion(&body, &*adapter)).expect("live atlascloud completion");
    assert!(
        usage.prompt_tokens > 0 && usage.completion_tokens > 0,
        "usage must parse from the OpenAI-compatible shape: {usage:?}"
    );
    eprintln!("live atlascloud ok — model={model} text={text:?} usage={usage:?}");
}

/// Live control for `atlascloud_live_completion_round_trips`.
///
/// Why: Fireworks reached its own endpoint before this PR, so if #3765's
/// `wire_model_id`/`requires_raw_http` refactor broke the prefix-stripping it
/// generalised, THIS is where it shows — an unchanged provider failing is a
/// regression, not a new-provider unknown.
/// What: same credential-gated skip and `#[ignore]` policy as above.
/// Test: itself.
#[test]
#[ignore = "requires FIREWORKS_API_KEY; skipped in CI"]
fn fireworks_live_completion_still_round_trips() {
    crate::test_env::force_env_local_loaded();
    if trusty_common::credentials::resolve_key("fireworks").is_none() {
        eprintln!("FIREWORKS_API_KEY not resolvable — skipping live test");
        return;
    }
    // Overridable for the same reason as the AtlasCloud test above, and with
    // an additional one: Fireworks RETIRES model ids (the long-standing
    // `llama-v3p1-8b-instruct` fixture now answers 404), so a hardcoded slug
    // rots into a false failure.
    let model = std::env::var("FIREWORKS_TEST_MODEL")
        .unwrap_or_else(|_| "accounts/fireworks/models/gpt-oss-20b".to_string());
    let slug = format!("fireworks/{model}");
    let slug = slug.as_str();
    let adapter = crate::llm::adapter::adapter_for_model(slug);
    let body = serde_json::json!({
        "model": adapter.wire_model_id(slug),
        "messages": [
            {"role": "system", "content": "You are a concise assistant."},
            {"role": "user", "content": "Reply with exactly the word: pong"}
        ],
        "max_tokens": 64,
        "temperature": 0.0,
    });
    let (text, _tool_calls, usage) =
        block_on(send_raw_completion(&body, &*adapter)).expect("live fireworks completion");
    let text = text.expect("assistant text");
    assert!(!text.trim().is_empty());
    assert!(usage.prompt_tokens > 0, "{usage:?}");
    eprintln!("live fireworks ok — model={model} text={text:?} usage={usage:?}");
}

// --- #5943: the shared LLM client is bounded ---------------------------------

/// Bind a loopback listener that accepts connections and then answers nothing.
///
/// Why: #5943's failure shape is a LIVE-but-unresponsive endpoint — the real
/// one was `ollama` LISTENing on :11434 while `curl -m 3` timed out against it.
/// A closed port is a different shape entirely (connection refused, which
/// already failed fast and is covered by `transport_error_detects_connect_failure`),
/// so this regression needs a socket that completes the handshake and then goes
/// silent. Binding one here keeps the test hermetic: it behaves identically
/// whether or not a real ollama is running on the machine.
/// What: binds `127.0.0.1:0`, spawns a task that accepts forever and parks each
/// accepted stream — never reading the request, never writing a byte — and
/// returns the bound `http://127.0.0.1:<port>` base URL.
/// Test: used by `llm_client_gives_up_on_a_listener_that_never_responds` and
/// `a_read_timeout_is_not_retried`.
async fn spawn_silent_listener() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("listener local addr");
    tokio::spawn(async move {
        let mut parked = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            // Hold the stream. Dropping it closes the connection and the client
            // sees a reset instead of the silence this test is about.
            parked.push(stream);
        }
    });
    format!("http://{addr}")
}

/// The client abandons a connection that is established but never answers.
///
/// Why: #5943 — `HTTP_CLIENT` was `reqwest::Client::new()`, which sets no
/// deadline, so this request never returned at all. The only thing bounding it
/// was a caller-supplied wrapper, and the one caller that had a wrapper threw
/// its result away. Run against the pre-fix client this test fails on the outer
/// `expect` below: the wrapper fires, which is exactly the rescue the fix has
/// to make unnecessary.
/// What: points a client built by `build_llm_client` — the same constructor
/// `http_client()` uses, with a sub-second budget instead of the production
/// five minutes — at a listener that accepts and stays silent, and asserts the
/// request ends in a timeout of the client's own making.
/// Test: itself.
#[tokio::test]
async fn llm_client_gives_up_on_a_listener_that_never_responds() {
    let url = spawn_silent_listener().await;
    let client = build_llm_client(Duration::from_millis(500), Duration::from_millis(300))
        .expect("build llm client");

    let started = std::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        client.post(format!("{url}/v1/chat/completions")).send(),
    )
    .await
    .expect("the client must give up on its own — this wrapper is the rescue #5943 removed")
    .expect_err("a listener that answers nothing must not yield a response");

    assert!(
        err.is_timeout(),
        "the request must end in a timeout, not some other transport error: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the read budget was 300ms; giving up took {:?}",
        started.elapsed()
    );
}

/// The process-wide client is built with the production LLM budget.
///
/// Why: #5943 — a `build_llm_client` that nobody wires into `HTTP_CLIENT`
/// fixes nothing, and that is the half of the defect a behavioural test on the
/// constructor cannot see. `reqwest::Client`'s `Debug` prints `read_timeout`
/// only when one is configured, so comparing the shared client against a
/// freshly built one pins both that a budget exists and that it is the intended
/// one. Against the pre-fix `OnceLock::get_or_init(reqwest::Client::new)` the
/// two render differently and this fails.
/// What: asserts `http_client()` renders identically to
/// `build_llm_client(LLM_CONNECT_TIMEOUT, LLM_READ_TIMEOUT)`.
/// Test: itself.
#[test]
fn shared_http_client_carries_the_llm_timeout_budget() {
    let expected = build_llm_client(LLM_CONNECT_TIMEOUT, LLM_READ_TIMEOUT)
        .expect("build llm client with the production budget");
    assert_eq!(
        format!("{:?}", http_client()),
        format!("{expected:?}"),
        "the shared client must carry the LLM connect/read budget, not reqwest defaults"
    );
}

/// A timeout is not retried, but is still recognised as a transport failure.
///
/// Why: #5943 — `with_llm_retry` makes four attempts, so a retryable timeout
/// multiplies the wait it was added to bound: twenty minutes against a wedged
/// endpoint rather than five. An endpoint that produced no byte for the whole
/// budget does not produce one on the immediate retry, so the classifier stops.
/// The second assertion guards the boundary that change runs along: #3766's
/// local-failure recovery keys off `is_transport_error`, which must keep
/// firing — otherwise a timed-out ollama would propagate its raw reqwest error
/// to the user instead of the explanation that path builds.
/// What: produces a genuine `reqwest` timeout against the silent listener,
/// wraps it the way the call sites do, and asserts `is_transient_anyhow_error`
/// is false while `is_transport_error` stays true.
/// Test: itself.
#[tokio::test]
async fn a_read_timeout_is_not_retried() {
    let url = spawn_silent_listener().await;
    let client = build_llm_client(Duration::from_millis(500), Duration::from_millis(200))
        .expect("build llm client");
    let err = client
        .post(format!("{url}/v1/chat/completions"))
        .send()
        .await
        .expect_err("a silent listener must time out");
    assert!(err.is_timeout(), "expected a timeout error, got: {err}");

    let wrapped = anyhow::Error::new(err).context("raw chat completion POST failed");
    assert!(
        !is_transient_anyhow_error(&wrapped),
        "a timeout must not be retried — each retry spends another full budget: {wrapped:#}"
    );
    assert!(
        is_transport_error(&wrapped),
        "a timeout is still a transport failure, or #3766's local recovery stops firing: {wrapped:#}"
    );
}
