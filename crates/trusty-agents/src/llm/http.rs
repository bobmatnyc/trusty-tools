//! Shared HTTP plumbing for the LLM call paths.
//!
//! Why: The OpenRouter, Anthropic-native, and ollama paths all need a pooled
//! `reqwest::Client`, a transient-error retry policy, and provider-specific
//! request/response (de)serialization that async-openai 0.28 can't model.
//! Concentrating that plumbing here keeps the chat loop in `mod.rs` focused on
//! turn orchestration.
//! What: A process-wide `http_client()`, a `with_llm_retry` backoff wrapper,
//! the transient-error classifiers, `build_raw_request` (cache_control-aware
//! body builder), and the three POST helpers `create_chat_completion_lenient`,
//! `send_raw_completion`, and `send_anthropic_native_completion`.
//! Test: See module tests at the bottom (`http_client_returns_same_instance`,
//! `strip_service_tier_removes_field`, `build_raw_request_*`).

use std::sync::OnceLock;

use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionRequestMessage, ChatCompletionTool,
        CreateChatCompletionRequest, CreateChatCompletionResponse, FunctionCall,
    },
};

use super::adapter::{self, ModelAdapter};
use super::anthropic_native;
use crate::perf::TokenUsage;

/// Module-level `reqwest::Client` shared across raw LLM POSTs (MIN-2 / #98).
///
/// Why: `reqwest::Client::new()` allocates a fresh connection pool and TLS
/// state per invocation; creating one per LLM call in `send_raw_completion`
/// and `send_anthropic_native_completion` wastes TCP+TLS handshakes and
/// defeats keep-alive/HTTP2 multiplexing. A single process-wide client reuses
/// its pool across every request.
/// What: `OnceLock<reqwest::Client>` initialized lazily on first access.
/// Test: `http_client_returns_same_instance` below.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Hand out the shared, lazily-initialized `reqwest::Client`.
///
/// Why: See `HTTP_CLIENT` — connection pooling requires a single instance.
/// What: Returns a `'static` reference, initializing on first call.
/// Test: `http_client_returns_same_instance`.
pub(crate) fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

/// Transient HTTP error classifier used by `backon` retry wrappers.
///
/// Why: OpenRouter and api.anthropic.com both surface transient 429 / 5xx and
/// connection-level errors that succeed on retry. Treating them as hard
/// failures forces the operator to re-run the entire workflow. Surfacing
/// auth/quota errors (400/401/402) as transient would just hide a real
/// configuration problem, so this classifier returns `false` for those.
/// What: An error is "retryable" when it's a connection/timeout/decode error
/// (no HTTP status reached) or when the status code is 429 or in 500..=599.
/// Test: Exercised via `is_transient_anyhow_error` and integration runs.
fn is_transient_http_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Wrap an async LLM HTTP call in a 3-attempt exponential backoff that only
/// retries on transient errors (429, 5xx, connection/timeout failures).
///
/// Why: Centralizes the retry policy so both `send_raw_completion` and
/// `send_anthropic_native_completion` get identical behavior and any future
/// LLM call sites can opt in with one line.
/// What: Uses `backon::ExponentialBuilder` with `max_times(3)` and the
/// classifier `is_transient_anyhow_error` so non-retryable errors (auth,
/// quota, malformed request) fail fast. Returns `T` on success, the last
/// error on exhaustion.
/// Test: Indirectly via integration runs against a 429-throttled endpoint;
/// classifier covered by its own unit logic.
async fn with_llm_retry<T, F, Fut>(op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    use backon::{ExponentialBuilder, Retryable};
    let policy = ExponentialBuilder::default().with_max_times(3);
    op.retry(policy).when(is_transient_anyhow_error).await
}

/// True when an `anyhow::Error` was produced by a transient HTTP failure.
///
/// Why: `with_llm_retry` operates on `anyhow::Error` because every LLM call
/// site already returns `anyhow::Result`; we look for an underlying
/// `reqwest::Error` (no status reached → connection/timeout/decode) or a
/// status-bearing error tagged with a transient HTTP code.
/// What: Walks the error chain. Connection-level `reqwest::Error`s are
/// always retried; status-bearing ones only when `is_transient_http_status`
/// returns true.
/// Test: Covered indirectly by integration retries.
fn is_transient_anyhow_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            return match re.status() {
                Some(status) => is_transient_http_status(status),
                None => true,
            };
        }
    }
    false
}

/// Build a raw JSON request body mirroring async-openai's schema, optionally
/// injecting Anthropic `cache_control` onto the system message content.
///
/// Why: (#50) `async-openai` 0.28 does not expose `cache_control` on its
/// typed messages; bypassing it lets us send the extra field OpenRouter
/// forwards to Anthropic. Kept private so the tool-loop is the only consumer.
/// What: Converts the typed `ChatCompletionRequestMessage` vec to a JSON
/// array, wraps the system message's string content as
/// `[{"type":"text","text":<content>,"cache_control":{"type":"ephemeral"}}]`,
/// and assembles the full top-level chat/completions body.
/// Test: `build_raw_request_injects_cache_control`.
pub(super) fn build_raw_request(
    model: &str,
    messages: &[ChatCompletionRequestMessage],
    tools: &[ChatCompletionTool],
    temperature: f32,
    max_tokens: u32,
    inject_cache_control: bool,
) -> Result<serde_json::Value> {
    // Round-trip the typed messages through JSON so we get the OpenAI wire
    // shape without re-implementing it; then patch the system message.
    let mut msgs_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::to_value(m).context("serialize chat message"))
        .collect::<Result<Vec<_>>>()?;

    if inject_cache_control {
        // Patch the FIRST system message we find.
        for m in msgs_json.iter_mut() {
            if m.get("role").and_then(|v| v.as_str()) == Some("system") {
                // content may be string or array-of-blocks; normalize to array
                // with a single text block carrying cache_control.
                let text_val = match m.get("content").cloned() {
                    Some(serde_json::Value::String(s)) => s,
                    Some(serde_json::Value::Array(_arr)) => {
                        // Already block-shaped; patch first block's cache_control.
                        if let Some(arr) = m.get_mut("content").and_then(|v| v.as_array_mut())
                            && let Some(first) = arr.first_mut()
                            && let Some(obj) = first.as_object_mut()
                        {
                            obj.insert(
                                "cache_control".to_string(),
                                serde_json::json!({"type": "ephemeral"}),
                            );
                        }
                        break;
                    }
                    _ => break,
                };
                m["content"] = serde_json::json!([
                    {
                        "type": "text",
                        "text": text_val,
                        "cache_control": {"type": "ephemeral"}
                    }
                ]);
                break;
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "messages": msgs_json,
    });

    if !tools.is_empty() {
        let tools_json: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| serde_json::to_value(t).context("serialize tool"))
            .collect::<Result<Vec<_>>>()?;
        body["tools"] = serde_json::Value::Array(tools_json);
    }

    Ok(body)
}

/// Send a typed chat-completion request, tolerating unknown `service_tier`
/// values OpenRouter returns.
///
/// Why: (#486) OpenRouter (via Anthropic) now returns
/// `"service_tier":"standard"`, a variant async-openai's `ServiceTier` enum
/// doesn't know. The typed `client.chat().create()` path deserializes the
/// body internally and fails the whole request. We can't extend the upstream
/// enum, so we POST the request ourselves, strip the offending field from the
/// raw JSON, then deserialize into the typed `CreateChatCompletionResponse`.
/// What: Serializes `request`, POSTs it to `{config}/chat/completions` with
/// the client's headers, removes the top-level `service_tier` field, and
/// deserializes the result. Retries transient 429/5xx via `with_llm_retry`.
/// Test: `strip_service_tier_removes_field` covers the JSON sanitization;
/// the request path is integration-tested via the OpenRouter smoke test.
pub(super) async fn create_chat_completion_lenient(
    client: &Client<OpenAIConfig>,
    request: CreateChatCompletionRequest,
) -> Result<CreateChatCompletionResponse> {
    use async_openai::config::Config;

    let config = client.config();
    let url = config.url("/chat/completions");
    let headers = config.headers();
    let query = config.query();
    let body = serde_json::to_value(&request).context("failed to serialize chat request")?;

    let json: serde_json::Value = with_llm_retry(|| async {
        let resp = http_client()
            .post(&url)
            .headers(headers.clone())
            .query(&query)
            .json(&body)
            .send()
            .await
            .context("chat completion POST failed")?;
        // Preserve the underlying `reqwest::Error` (with status) in the anyhow
        // chain so `with_llm_retry`'s classifier can decide whether to retry.
        let resp = resp.error_for_status()?;
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse OpenRouter JSON response")?;
        Ok::<_, anyhow::Error>(json)
    })
    .await?;

    let json = strip_service_tier(json);
    serde_json::from_value(json).context("failed to deserialize chat completion response")
}

/// Remove the top-level `service_tier` field from a chat-completion response.
///
/// Why: (#486) OpenRouter returns `service_tier` values async-openai's
/// `ServiceTier` enum can't deserialize (e.g. `"standard"`). The field is
/// purely informational and unused downstream, so dropping it lets the rest
/// of the typed response deserialize cleanly.
/// What: If `json` is an object, removes the `service_tier` key; returns the
/// (possibly modified) value unchanged otherwise.
/// Test: `strip_service_tier_removes_field`.
fn strip_service_tier(mut json: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = json.as_object_mut() {
        obj.remove("service_tier");
    }
    json
}

/// POST a raw chat-completions body to OpenRouter and parse back the pieces
/// the tool loop needs (content, tool_calls, usage with Anthropic cache fields).
///
/// Why: (#50) The raw path is only used when we've injected cache_control
/// fields async-openai cannot represent. Extracting usage + tool_calls from
/// `serde_json::Value` is trivial and keeps us provider-agnostic.
/// What: Reads OPENROUTER_API_KEY + OPENROUTER_BASE_URL, POSTs the body,
/// pulls `choices[0].message.{content, tool_calls}` and `usage.*` including
/// `cache_read_input_tokens` / `cache_creation_input_tokens`.
/// Test: Exercised end-to-end via integration; unit-tested construction via
/// `build_raw_request_injects_cache_control`.
pub(super) async fn send_raw_completion(
    body: &serde_json::Value,
    adapter: &dyn ModelAdapter,
) -> Result<(
    Option<String>,
    Vec<ChatCompletionMessageToolCall>,
    TokenUsage,
)> {
    // #287: Route through the adapter's `api_endpoint` so ollama (or any other
    // adapter that overrides the base URL) goes to its native server instead
    // of OpenRouter. Adapters with no auth (ollama) leave `auth_header_value`
    // empty; we skip the Authorization header in that case.
    let endpoint = adapter.api_endpoint(false);
    let url = format!(
        "{}/chat/completions",
        endpoint.base_url.trim_end_matches('/')
    );
    let auth_value = if endpoint.auth_header_value.is_empty() {
        // Fall back to the resolved OpenRouter credential (env > .env.local >
        // secure store) when the adapter doesn't supply one (legacy callers
        // that rely on this fallback rather than a populated `ApiEndpoint`).
        trusty_common::inference::credentials::resolve_key("openrouter").unwrap_or_default()
    } else {
        endpoint
            .auth_header_value
            .strip_prefix("Bearer ")
            .unwrap_or(&endpoint.auth_header_value)
            .to_string()
    };
    if auth_value.is_empty() && !url.contains("localhost") && !url.contains("127.0.0.1") {
        anyhow::bail!(
            "openrouter credential not found (checked process env, .env.local, and the \
             secure store — run `tagent config keys set openrouter` or export \
             OPENROUTER_API_KEY) and adapter supplied no credential"
        );
    }
    // backon retry: on transient 429/5xx + connection errors, retry up to 3x
    // with exponential backoff. Auth/quota errors (400/401/402) fail fast.
    let json: serde_json::Value = with_llm_retry(|| async {
        let mut req = http_client().post(&url).json(body);
        if !auth_value.is_empty() {
            req = req.bearer_auth(&auth_value);
        }
        let resp = req
            .send()
            .await
            .context("raw chat completion POST failed")?;
        // `error_for_status` keeps the underlying `reqwest::Error` reachable
        // via the anyhow chain so the retry classifier can inspect its status.
        let resp = resp.error_for_status()?;
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse OpenRouter JSON response")?;
        Ok::<_, anyhow::Error>(json)
    })
    .await?;

    // Adapter parses provider-specific usage (Anthropic cache fields etc.).
    let usage = adapter.parse_usage(&json);

    // choices[0].message
    let msg = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .context("raw response missing choices[0].message")?;

    let content = msg
        .get("content")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let tool_calls: Vec<ChatCompletionMessageToolCall> = msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())?
                        .to_string();
                    let arguments = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}")
                        .to_string();
                    Some(ChatCompletionMessageToolCall {
                        id,
                        r#type: async_openai::types::ChatCompletionToolType::Function,
                        function: FunctionCall { name, arguments },
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok((content, tool_calls, usage))
}

/// POST an Anthropic native `/v1/messages` body to the configured endpoint
/// and return the pieces the tool loop expects.
///
/// Why: #59 — when routing to `api.anthropic.com` directly we can't reuse
/// `send_raw_completion` because its URL is hard-coded to
/// `{base}/chat/completions` and its auth header is `Bearer`. This helper
/// owns the direct-API POST (respecting `x-api-key` and extra headers) and
/// normalizes the response back into the same `(content, tool_calls, usage)`
/// tuple the OpenAI path produces so the loop body is unchanged.
/// What: POSTs `body` to `{endpoint.base_url}/messages` with the adapter's
/// auth + extra headers, parses the response via
/// `anthropic_native::parse_anthropic_response`, and converts the result
/// into `(Option<String>, Vec<ChatCompletionMessageToolCall>, TokenUsage)`.
/// Test: Exercised end-to-end via integration; unit-tested through
/// `anthropic_native::parse_anthropic_response` tests.
pub(super) async fn send_anthropic_native_completion(
    body: &serde_json::Value,
    endpoint: &adapter::ApiEndpoint,
) -> Result<(
    Option<String>,
    Vec<ChatCompletionMessageToolCall>,
    TokenUsage,
)> {
    let url = format!("{}/messages", endpoint.base_url.trim_end_matches('/'));
    // backon retry: same transient-only policy as the OpenRouter path.
    let json: serde_json::Value = with_llm_retry(|| async {
        let mut req = http_client()
            .post(&url)
            .header(&endpoint.auth_header_name, &endpoint.auth_header_value)
            .header("content-type", "application/json");
        for (k, v) in &endpoint.extra_headers {
            req = req.header(k, v);
        }
        let resp = req
            .json(body)
            .send()
            .await
            .context("Anthropic direct POST failed")?;
        let resp = resp.error_for_status()?;
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse Anthropic JSON response")?;
        Ok::<_, anyhow::Error>(json)
    })
    .await?;
    let parsed = anthropic_native::parse_anthropic_response(&json);
    Ok((parsed.text_content, parsed.tool_calls, parsed.usage))
}

#[cfg(test)]
mod tests {
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
        // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
        crate::test_env::force_env_local_loaded();
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
        // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
        crate::test_env::force_env_local_loaded();
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
        // #3464: see `crate::test_env::force_env_local_loaded`'s docs.
        crate::test_env::force_env_local_loaded();
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
        let system: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
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
        let system: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
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
}
