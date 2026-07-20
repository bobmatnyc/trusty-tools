//! `GET /api/models` tests (#3243).
//!
//! Why: The catalog endpoint's entire safety contract is "never leak a
//! credential value" — the shape and the ollama-absent path matter too, but
//! a regression that started echoing a resolved key into the JSON body would
//! be far worse than a wrong `context_window`. These tests weight
//! accordingly: one dedicated leak-proof test with a real (fake) secret in
//! the env, plus shape/stability coverage.
//! What: `models_response_has_expected_shape`, `reachable_today_matches_documented_set`,
//! `credential_values_never_serialized`, `zero_credentials_configured_is_stable`,
//! `ollama_absent_reports_available_false`.
//! Test: This module IS the test.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serial_test::serial;
use tower::ServiceExt;

use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

/// Provider env vars the resolver's env tier checks, mirrored from
/// `trusty_common::inference::credentials::env_var_for`.
const CREDENTIAL_ENV_VARS: &[&str] = &[
    "FIREWORKS_API_KEY",
    "OPENROUTER_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "TOGETHER_API_KEY",
    "ATLASCLOUD_API_KEY",
];

fn router() -> Router {
    build_router(AppState::default())
}

async fn get_models_body(app: Router) -> serde_json::Value {
    let req = Request::builder()
        .uri("/api/models")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Why: The picker needs every registry provider represented with the exact
/// fields the frontend reads, plus the synthetic `local` entry.
/// What: Asserts `providers` has one entry per
/// `trusty_common::inference::registry::all()` provider, each carrying the
/// five documented fields with the right JSON types, and `local` carries
/// `provider_id: "ollama"` + a non-empty `default_model` + boolean `available`
/// / `reachable_today`.
#[tokio::test]
async fn models_response_has_expected_shape() {
    let body = get_models_body(router()).await;

    let providers = body["providers"].as_array().expect("providers array");
    let expected_len = trusty_common::inference::registry::all().len();
    assert_eq!(providers.len(), expected_len);

    for entry in providers {
        assert!(entry["provider_id"].is_string(), "entry: {entry}");
        assert!(entry["default_model"].is_string(), "entry: {entry}");
        assert!(entry["context_window"].is_u64(), "entry: {entry}");
        assert!(
            entry["credential_configured"].is_boolean(),
            "entry: {entry}"
        );
        assert!(entry["reachable_today"].is_boolean(), "entry: {entry}");
    }

    let local = &body["local"];
    assert_eq!(local["provider_id"], "ollama");
    assert!(
        local["default_model"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(local["available"].is_boolean());
    assert!(local["reachable_today"].is_boolean());
}

/// Why: #2410 tracks migrating the legacy chat dispatch onto the unified
/// registry; until it fully lands the picker must not claim a provider is
/// usable when `crate::llm::adapter::adapter_for_model` has no branch for
/// it. This pins the documented set so an accidental edit to
/// `models::reachable_today` is caught here rather than silently shipping a
/// wrong claim to the UI.
/// What: OpenRouter/Bedrock/Anthropic/Fireworks (#2410 Step 3, via
/// `FireworksAdapter`) are `true`; OpenAI/Together/AtlasCloud/registry-`local`
/// are `false`. Ollama's synthetic `local.reachable_today` (a DIFFERENT
/// field — the top-level `local` object, not the `providers` array's `local`
/// entry) is always `true` (it has its own legacy `ollama/`-prefix branch,
/// independent of #2410).
#[tokio::test]
async fn reachable_today_matches_documented_set() {
    let body = get_models_body(router()).await;
    let providers = body["providers"].as_array().unwrap();

    let reachable_today = |id: &str| -> bool {
        providers
            .iter()
            .find(|e| e["provider_id"] == id)
            .unwrap_or_else(|| panic!("missing provider {id}"))["reachable_today"]
            .as_bool()
            .unwrap()
    };

    assert!(reachable_today("openrouter"));
    assert!(reachable_today("bedrock"));
    assert!(reachable_today("anthropic"));
    assert!(reachable_today("fireworks"));
    assert!(!reachable_today("openai"));
    assert!(!reachable_today("together"));
    assert!(!reachable_today("atlascloud"));
    // The registry's `local` provider (#3247) isn't wired into the legacy
    // dispatch under its OWN name — only the synthetic `local.provider_id ==
    // "ollama"` entry below is (via the `ollama/` prefix).
    assert!(!reachable_today("local"));

    assert_eq!(body["local"]["reachable_today"], true);
}

/// Why: The single safety-critical property of this endpoint — a resolved
/// credential must never cross the HTTP boundary, even when one genuinely
/// resolves. Sets a fake `ANTHROPIC_API_KEY`; the resolver's env tier
/// short-circuits before ever touching the (unsandboxed, possibly
/// real-credential-bearing) secure store, so this deterministically makes
/// anthropic resolve `true` without needing `$HOME` sandboxing — see
/// `zero_credentials_configured_is_stable`'s doc comment for why sandboxing
/// `$HOME` in this crate's test binary is NOT reliable (a handful of
/// pre-existing tests elsewhere mutate `$HOME` without the shared lock).
/// Asserts the raw response body never contains the fake value anywhere,
/// while `credential_configured` still flips `true` for anthropic — proving
/// the boolean is computed correctly, not just absent because nothing
/// resolved.
/// What: Holds `ENV_LOCK` (the only global mutation this test performs) for
/// its full body; restores the env var on the way out. Also `#[serial]`:
/// `llm::adapter::tests`, `llm::http::tests`, and other files mutate the same
/// credential env vars from tests that can only join the crate-wide `#[serial]`
/// group (not `ENV_LOCK`, e.g. `#[tokio::test]`s that can't hold a
/// `std::sync::Mutex` guard across `.await`), so this test must carry
/// `#[serial]` too to stay mutually exclusive with them.
#[tokio::test]
#[serial]
async fn credential_values_never_serialized() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let prev_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
    const FAKE_SECRET: &str = "sk-ant-test-fake-secret-3243-do-not-leak";
    // SAFETY: ENV_LOCK held for the entire test body.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", FAKE_SECRET);
    }

    let bytes = {
        let req = Request::builder()
            .uri("/api/models")
            .body(Body::empty())
            .unwrap();
        let resp = router().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap()
    };
    let raw = String::from_utf8_lossy(&bytes);
    assert!(
        !raw.contains(FAKE_SECRET),
        "response body leaked the credential value: {raw}"
    );

    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let anthropic = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["provider_id"] == "anthropic")
        .expect("anthropic entry present");
    assert_eq!(anthropic["credential_configured"], true);

    // SAFETY: ENV_LOCK still held.
    unsafe {
        match prev_anthropic {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }
}

/// Why: The GUI's picker must render sensibly on a fresh install with no
/// *env-tier* credentials configured — this pins that the endpoint never
/// errors with every credential env var cleared. It intentionally does NOT
/// assert every provider is `false`: this crate's test binary has a handful
/// of pre-existing tests (outside this module) that sandbox `$HOME` without
/// holding `crate::test_env::HOME_LOCK`, so a `$HOME`-swap here can race
/// against them and transiently see either the real secure store (on a dev
/// machine that has one configured, as observed while writing this test) or
/// a sandboxed empty one — asserting a fixed `false` would be flaky either
/// way. Instead this test asserts *self-consistency*: whatever
/// `resolve_key` (the exact function the handler calls) returns at the same
/// moment, for each provider, is exactly what the response reports — which
/// is the actually-load-bearing contract (the handler doesn't invert or
/// fabricate the boolean) and is fully deterministic regardless of what's in
/// the store.
/// What: Clears every credential env var, then asserts each provider's
/// `credential_configured` equals
/// `id == "bedrock" || id == "local" || resolve_key(id).is_some()`.
#[tokio::test]
#[serial]
async fn zero_credentials_configured_is_stable() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let prev_vars: Vec<(&str, Option<std::ffi::OsString>)> = CREDENTIAL_ENV_VARS
        .iter()
        .map(|&k| (k, std::env::var_os(k)))
        .collect();
    // SAFETY: ENV_LOCK held for the entire test body.
    unsafe {
        for &k in CREDENTIAL_ENV_VARS {
            std::env::remove_var(k);
        }
    }

    let body = get_models_body(router()).await;
    for entry in body["providers"].as_array().unwrap() {
        let id = entry["provider_id"].as_str().unwrap();
        // Bedrock (AWS chain) and Local (unauthenticated localhost, #3247)
        // both have `credential_name() == None` — the handler's
        // `credential_configured` reports `true` unconditionally for either.
        let expected = id == "bedrock"
            || id == "local"
            || trusty_common::inference::credentials::resolve_key(id).is_some();
        assert_eq!(
            entry["credential_configured"], expected,
            "provider {id} credential_configured mismatch: {entry}"
        );
    }

    // SAFETY: ENV_LOCK still held.
    unsafe {
        for (k, v) in prev_vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// Why: The endpoint must degrade gracefully when ollama isn't running (the
/// common case in CI) and must never turn into a 500 or a hang — but it must
/// equally not lie and report `false` on a developer's machine that actually
/// has `ollama serve` up. Asserting a hard-coded `false` here would be
/// flaky depending on the environment (caught during implementation: this
/// exact assertion failed on a dev machine with ollama running); asserting
/// *consistency with the live (cached) probe* is the environment-agnostic
/// version of the same contract — in CI, where ollama is absent, `expected`
/// evaluates to `false`, which is the "absent path" case this test name
/// documents.
/// What: Computes the same `is_ollama_available_cached` call the handler
/// uses and asserts the response's `local.available` matches it exactly,
/// plus that the request never errors.
#[tokio::test]
async fn ollama_absent_reports_available_false() {
    let body = get_models_body(router()).await;
    let host =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let expected = crate::local_inference::is_ollama_available_cached(&host).await;
    assert_eq!(body["local"]["available"], expected);
    assert_eq!(body["local"]["provider_id"], "ollama");
}
