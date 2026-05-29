//! Unit tests for the model-provider adapters.
//!
//! Why: Keeping the (sizeable) test suite out of `mod.rs` keeps the public
//! API surface under the 500-line cap while preserving full coverage.
//! What: Routing, tool-choice shapes, cache-control injection, usage parsing,
//! and endpoint resolution (including the #62 direct-API auth matrix).
//! Test: This IS the test module.

use super::*;
use serde_json::json;

#[test]
fn adapter_for_model_routes_anthropic() {
    let a = adapter_for_model("anthropic/claude-sonnet-4-6");
    assert_eq!(a.provider(), Provider::Anthropic);
    let a = adapter_for_model("claude-haiku-4");
    assert_eq!(a.provider(), Provider::Anthropic);
}

#[test]
fn adapter_for_model_routes_openai() {
    assert_eq!(
        adapter_for_model("openai/gpt-4.1").provider(),
        Provider::OpenAI
    );
    assert_eq!(
        adapter_for_model("openai/o3-mini").provider(),
        Provider::OpenAI
    );
    assert_eq!(adapter_for_model("gpt-4o").provider(), Provider::OpenAI);
}

#[test]
fn adapter_for_model_routes_bedrock() {
    let a = adapter_for_model("bedrock/anthropic.claude-3-5-haiku-20241022-v1:0");
    assert_eq!(a.provider(), Provider::Bedrock);
    // Even an Anthropic-flavored model id under bedrock/ must NOT route
    // to AnthropicAdapter — the SDK path is required.
    let a = adapter_for_model("bedrock/anthropic.claude-3-opus-20240229-v1:0");
    assert_eq!(a.provider(), Provider::Bedrock);
}

#[test]
fn bedrock_adapter_strips_prefix() {
    let a = BedrockAdapter {
        model_id: "anthropic.claude-3-5-haiku-20241022-v1:0".to_string(),
    };
    assert_eq!(a.model_id, "anthropic.claude-3-5-haiku-20241022-v1:0");
    let ep = a.api_endpoint(true);
    assert_eq!(ep.auth_source, AuthSource::Bedrock);
}

#[test]
fn adapter_for_model_routes_generic_for_unknown() {
    assert_eq!(
        adapter_for_model("google/gemini-2.5-flash").provider(),
        Provider::Generic
    );
}

#[test]
fn anthropic_tool_choice_any_shape() {
    let a = AnthropicAdapter;
    assert_eq!(a.tool_choice_any().unwrap(), json!({"type": "any"}));
}

#[test]
fn openai_tool_choice_any_shape() {
    let a = OpenAiAdapter;
    assert_eq!(a.tool_choice_any().unwrap(), json!("required"));
}

#[test]
fn generic_tool_choice_any_is_none() {
    let a = GenericAdapter;
    assert!(a.tool_choice_any().is_none());
}

#[test]
fn tool_choice_auto_shapes() {
    assert_eq!(AnthropicAdapter.tool_choice_auto().unwrap(), json!("auto"));
    assert_eq!(OpenAiAdapter.tool_choice_auto().unwrap(), json!("auto"));
    assert!(GenericAdapter.tool_choice_auto().is_none());
}

#[test]
fn anthropic_inject_cache_control_on_string_system() -> anyhow::Result<()> {
    // When the request body has a top-level `system: "text"`, it should be
    // expanded into a content-block array carrying cache_control.
    let mut body = json!({
        "model": "anthropic/claude-sonnet-4-6",
        "system": "You are helpful.",
        "messages": [{"role": "user", "content": "hi"}]
    });
    AnthropicAdapter.inject_cache_control(&mut body, true);
    let sys = body
        .get("system")
        .ok_or_else(|| anyhow::anyhow!("expected `system` key on body, got: {:?}", body))?;
    let arr = sys.as_array().ok_or_else(|| {
        anyhow::anyhow!(
            "expected system to be array after injection, got: {:?}",
            sys
        )
    })?;
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "You are helpful.");
    assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    Ok(())
}

#[test]
fn anthropic_inject_cache_control_on_messages_system() -> anyhow::Result<()> {
    // OpenAI-style body: system lives inside messages.
    let mut body = json!({
        "model": "anthropic/claude-sonnet-4-6",
        "messages": [
            {"role": "system", "content": "sys prompt"},
            {"role": "user", "content": "hi"}
        ]
    });
    AnthropicAdapter.inject_cache_control(&mut body, true);
    let msgs = body["messages"].as_array().ok_or_else(|| {
        anyhow::anyhow!("expected messages to be array, got: {:?}", body["messages"])
    })?;
    let sys = &msgs[0];
    let content = sys["content"].as_array().ok_or_else(|| {
        anyhow::anyhow!(
            "expected system message content to be array after injection, got: {:?}",
            sys["content"]
        )
    })?;
    assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
    Ok(())
}

#[test]
fn anthropic_inject_cache_control_noop_when_disabled() {
    let mut body = json!({"system": "hi"});
    AnthropicAdapter.inject_cache_control(&mut body, false);
    assert_eq!(body["system"], json!("hi"));
}

#[test]
fn openai_inject_cache_control_is_noop() {
    let mut body = json!({"messages":[{"role":"system","content":"x"}]});
    let before = body.clone();
    OpenAiAdapter.inject_cache_control(&mut body, true);
    assert_eq!(before, body);
}

#[test]
fn anthropic_parse_usage_native_shape() {
    let resp = json!({
        "usage": {
            "input_tokens": 1000,
            "output_tokens": 200,
            "cache_read_input_tokens": 400,
            "cache_creation_input_tokens": 100
        }
    });
    let u = AnthropicAdapter.parse_usage(&resp);
    assert_eq!(u.prompt_tokens, 1000);
    assert_eq!(u.completion_tokens, 200);
    assert_eq!(u.cache_read_tokens, 400);
    assert_eq!(u.cache_creation_tokens, 100);
}

#[test]
fn openai_parse_usage_shape() {
    let resp = json!({
        "usage": {
            "prompt_tokens": 900,
            "completion_tokens": 150,
            "prompt_tokens_details": {"cached_tokens": 300}
        }
    });
    let u = OpenAiAdapter.parse_usage(&resp);
    assert_eq!(u.prompt_tokens, 900);
    assert_eq!(u.completion_tokens, 150);
    assert_eq!(u.cache_read_tokens, 300);
    assert_eq!(u.cache_creation_tokens, 0);
}

#[test]
fn generic_parse_usage_tries_both_shapes() {
    let openai_shape = json!({"usage":{"prompt_tokens":10,"completion_tokens":5}});
    let u = GenericAdapter.parse_usage(&openai_shape);
    assert_eq!(u.prompt_tokens, 10);
    assert_eq!(u.completion_tokens, 5);

    let anthropic_shape = json!({"usage":{"input_tokens":30,"output_tokens":7}});
    let u = GenericAdapter.parse_usage(&anthropic_shape);
    assert_eq!(u.prompt_tokens, 30);
    assert_eq!(u.completion_tokens, 7);
}

#[test]
fn supports_thinking_only_anthropic() {
    assert!(AnthropicAdapter.supports_thinking());
    assert!(!OpenAiAdapter.supports_thinking());
    assert!(!GenericAdapter.supports_thinking());
}

// The api_endpoint tests mutate process-global env. Serialize them behind
// a mutex so they don't race with each other or with `agents::mod::tests`.
use std::sync::Mutex;
static ENDPOINT_ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_env<F: FnOnce()>(kvs: &[(&str, Option<&str>)], f: F) {
    let _g = ENDPOINT_ENV_LOCK.lock().unwrap();
    // Snapshot previous values to restore afterwards.
    let prev: Vec<_> = kvs
        .iter()
        .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
        .collect();
    // SAFETY: test helper, single-threaded under ENDPOINT_ENV_LOCK.
    unsafe {
        for (k, v) in kvs {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
    f();
    // SAFETY: same lock still held.
    unsafe {
        for (k, v) in prev {
            match v {
                Some(val) => std::env::set_var(&k, val),
                None => std::env::remove_var(&k),
            }
        }
    }
}

#[test]
fn anthropic_api_endpoint_direct_when_key_set() {
    with_env(
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", None),
            ("ANTHROPIC_API_KEY", Some("sk-ant-test123")),
            ("ANTHROPIC_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(true);
            assert!(
                ep.base_url.contains("api.anthropic.com"),
                "unexpected base_url: {}",
                ep.base_url
            );
            assert_eq!(ep.auth_header_name, "x-api-key");
            assert_eq!(ep.auth_header_value, "sk-ant-test123");
            assert!(
                ep.extra_headers
                    .iter()
                    .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"),
                "missing anthropic-version header: {:?}",
                ep.extra_headers
            );
            assert_eq!(ep.auth_source, AuthSource::AnthropicApiKey);
        },
    );
}

#[test]
fn anthropic_api_endpoint_use_direct_false_goes_to_openrouter() {
    with_env(
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", None),
            ("ANTHROPIC_API_KEY", Some("sk-ant-test123")),
            ("OPENROUTER_API_KEY", Some("sk-or-v1-test")),
            ("OPENROUTER_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(false);
            assert!(
                ep.base_url.contains("openrouter.ai"),
                "unexpected base_url: {}",
                ep.base_url
            );
            assert_eq!(ep.auth_header_name, "Authorization");
            assert!(ep.auth_header_value.starts_with("Bearer "));
        },
    );
}

#[test]
fn anthropic_api_endpoint_falls_back_to_openrouter_without_key() {
    with_env(
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", None),
            ("ANTHROPIC_API_KEY", None),
            ("OPENROUTER_API_KEY", Some("sk-or-v1-test")),
            ("OPENROUTER_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(true);
            assert!(
                ep.base_url.contains("openrouter.ai"),
                "unexpected base_url: {}",
                ep.base_url
            );
            assert_eq!(ep.auth_header_name, "Authorization");
        },
    );
}

#[test]
fn openai_api_endpoint_always_openrouter() {
    with_env(
        &[
            ("ANTHROPIC_API_KEY", Some("sk-ant-test")),
            ("OPENROUTER_API_KEY", Some("sk-or-v1-test")),
            ("OPENROUTER_BASE_URL", None),
        ],
        || {
            let ep = OpenAiAdapter.api_endpoint(true);
            assert!(
                ep.base_url.contains("openrouter.ai"),
                "OpenAI should never route direct-Anthropic: got {}",
                ep.base_url
            );
            assert_eq!(ep.auth_header_name, "Authorization");
        },
    );
}

#[test]
fn generic_api_endpoint_is_openrouter() {
    with_env(
        &[
            ("OPENROUTER_API_KEY", Some("sk-or-v1-test")),
            ("OPENROUTER_BASE_URL", None),
        ],
        || {
            let ep = GenericAdapter.api_endpoint(true);
            assert!(ep.base_url.contains("openrouter.ai"));
        },
    );
}

#[test]
fn anthropic_uses_native_format_true() {
    assert!(AnthropicAdapter.uses_native_format());
}

#[test]
fn openai_uses_native_format_false() {
    assert!(!OpenAiAdapter.uses_native_format());
    assert!(!GenericAdapter.uses_native_format());
}

// --- #62: Direct API auth tests ---
// CLAUDE_CODE_OAUTH_TOKEN must NOT be used for direct API routing.
// sk-ant-oat01-* tokens are rejected by api.anthropic.com with 401.
// OAuth tokens are only valid for runner="claude-code" agents.

#[test]
fn oauth_token_is_not_used_for_direct_api_routing() {
    // Even when CLAUDE_CODE_OAUTH_TOKEN is set, it must NOT route to
    // api.anthropic.com. The token is for ClaudeCodeAgentRunner only.
    // With no ANTHROPIC_API_KEY, we expect OpenRouter fallback.
    with_env(
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", Some("sk-ant-oat01-fake-oauth")),
            ("ANTHROPIC_API_KEY", None),
            ("OPENROUTER_API_KEY", Some("sk-or-v1-test")),
            ("OPENROUTER_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(true);
            assert_eq!(
                ep.auth_source,
                AuthSource::OpenRouter,
                "OAuth token must NOT produce direct API endpoint"
            );
            assert!(
                ep.base_url.contains("openrouter.ai"),
                "expected openrouter.ai fallback, got: {}",
                ep.base_url
            );
        },
    );
}

#[test]
fn oauth_token_present_with_api_key_uses_api_key() {
    // When both CLAUDE_CODE_OAUTH_TOKEN and ANTHROPIC_API_KEY are set,
    // the OAuth token is ignored and ANTHROPIC_API_KEY is used for direct API.
    with_env(
        &[
            (
                "CLAUDE_CODE_OAUTH_TOKEN",
                Some("sk-ant-oat01-should-be-ignored"),
            ),
            ("ANTHROPIC_API_KEY", Some("sk-ant-real-key")),
            ("ANTHROPIC_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(true);
            assert_eq!(ep.auth_source, AuthSource::AnthropicApiKey);
            assert_eq!(ep.auth_header_name, "x-api-key");
            assert_eq!(ep.auth_header_value, "sk-ant-real-key");
            assert!(
                ep.base_url.contains("api.anthropic.com"),
                "expected api.anthropic.com, got: {}",
                ep.base_url
            );
        },
    );
}

#[test]
fn api_key_used_when_no_oauth_token() {
    // When only ANTHROPIC_API_KEY is set (no OAuth token), the adapter
    // must use the API-key path with x-api-key header.
    with_env(
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", None),
            ("ANTHROPIC_API_KEY", Some("sk-ant-only-key")),
            ("ANTHROPIC_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(true);
            assert_eq!(ep.auth_source, AuthSource::AnthropicApiKey);
            assert_eq!(ep.auth_header_name, "x-api-key");
            assert_eq!(ep.auth_header_value, "sk-ant-only-key");
        },
    );
}

#[test]
fn falls_back_to_openrouter_when_use_direct_false() {
    // Regardless of which Anthropic credentials are present, use_direct=false
    // must route through OpenRouter.
    with_env(
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", Some("oauth-tok-abc")),
            ("ANTHROPIC_API_KEY", Some("sk-ant-test")),
            ("OPENROUTER_API_KEY", Some("sk-or-v1-test")),
            ("OPENROUTER_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(false);
            assert_eq!(ep.auth_source, AuthSource::OpenRouter);
            assert!(
                ep.base_url.contains("openrouter.ai"),
                "expected openrouter.ai, got: {}",
                ep.base_url
            );
        },
    );
}

#[test]
fn empty_oauth_token_with_api_key_uses_api_key() {
    // An empty CLAUDE_CODE_OAUTH_TOKEN is irrelevant; ANTHROPIC_API_KEY is used.
    with_env(
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", Some("")),
            ("ANTHROPIC_API_KEY", Some("sk-ant-fallback")),
            ("ANTHROPIC_BASE_URL", None),
        ],
        || {
            let ep = AnthropicAdapter.api_endpoint(true);
            assert_eq!(ep.auth_source, AuthSource::AnthropicApiKey);
            assert_eq!(ep.auth_header_name, "x-api-key");
            assert_eq!(ep.auth_header_value, "sk-ant-fallback");
        },
    );
}
