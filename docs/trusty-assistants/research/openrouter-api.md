# OpenRouter API — Research Report

**Date:** 2026-04-22  
**Scope:** OpenRouter API for open-mpm: chat completions, streaming, tool calling, routing, Claude Sonnet 4.6 specifics

---

## Overview

OpenRouter is an LLM routing layer that normalizes the schema across models and providers to comply with the OpenAI Chat API. One schema works for all supported models. The endpoint is:

```
POST https://openrouter.ai/api/v1/chat/completions
```

Authentication:
```
Authorization: Bearer <OPENROUTER_API_KEY>
```

Recommended headers:
```
HTTP-Referer: <your-app-url>   # optional, identifies your app
X-OpenRouter-Title: <app-name> # optional, display name on dashboard
Content-Type: application/json
```

---

## Chat Completions

### Non-Streaming Request

```json
{
  "model": "anthropic/claude-sonnet-4.6",
  "messages": [
    { "role": "system", "content": "You are a helpful assistant." },
    { "role": "user", "content": "Hello!" }
  ],
  "max_completion_tokens": 4096
}
```

Notes:
- `max_tokens` is deprecated; use `max_completion_tokens`
- `response_format: { "type": "json_object" }` enforces JSON output
- `choices` is always an array even for single completions
- Each choice has a `message` property for non-streaming responses

### Streaming Request

Add `"stream": true` to the request body. Response is Server-Sent Events (SSE):

```
: OPENROUTER PROCESSING
data: {"id":"...","choices":[{"delta":{"content":"Hello"},"index":0}]}
data: {"id":"...","choices":[{"delta":{"content":"!"},"index":0}]}
data: {"id":"...","choices":[{"delta":{},"index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}
data: [DONE]
```

Key SSE details:
- `: OPENROUTER PROCESSING` comments may appear to prevent connection timeout — safely ignore
- Stream terminates with `data: [DONE]`
- Usage stats appear in the **final chunk** before `[DONE]`
- Generation ID is in the `X-Generation-Id` response header

### Error Handling During Streaming

- **Before any tokens streamed:** Standard JSON error response with HTTP 4xx/5xx
- **After tokens already streamed:** Error embedded as SSE event with `"finish_reason": "error"`, HTTP 200 (headers already sent)

Error HTTP codes:
- `400` — Bad request / malformed
- `401` — Invalid API key
- `402` — Insufficient credits
- `429` — Rate limit exceeded
- `502` — Model provider error
- `503` — Model unavailable

---

## Tool / Function Calling

OpenRouter supports OpenAI-compatible tool calling.

### Request Format

```json
{
  "model": "anthropic/claude-sonnet-4.6",
  "messages": [
    { "role": "user", "content": "What's the weather in Paris?" }
  ],
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "get_weather",
        "description": "Get current weather for a city",
        "parameters": {
          "type": "object",
          "properties": {
            "city": { "type": "string", "description": "City name" }
          },
          "required": ["city"]
        }
      }
    }
  ],
  "tool_choice": "auto"
}
```

The `tools` parameter must be included in **every request** in the multi-turn tool-call loop, not just the first one.

### Model Response (Tool Call)

```json
{
  "choices": [{
    "message": {
      "role": "assistant",
      "content": null,
      "tool_calls": [{
        "id": "call_abc123",
        "type": "function",
        "function": {
          "name": "get_weather",
          "arguments": "{\"city\":\"Paris\"}"
        }
      }]
    },
    "finish_reason": "tool_calls"
  }]
}
```

### Returning Tool Results

```json
{
  "role": "tool",
  "tool_call_id": "call_abc123",
  "content": "{\"temperature\": 18, \"condition\": \"cloudy\"}"
}
```

### Tool Calling Parameters

| Parameter | Values | Default | Description |
|-----------|--------|---------|-------------|
| `tool_choice` | `"auto"`, `"none"`, `{"type":"function","function":{"name":"..."}}` | `"auto"` | Control when tools are used |
| `parallel_tool_calls` | `true`/`false` | `true` | Allow multiple simultaneous tool calls |

### Streaming with Tool Calls

When streaming, tool calls arrive incrementally via `delta.tool_calls` chunks:

```
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"get_weather","arguments":""}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}
data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}
```

Accumulate `delta.tool_calls[i].function.arguments` strings across chunks, then parse the complete JSON.

### Interleaved Thinking

OpenRouter supports `reasoning`/`thinking` parameters for models that expose chain-of-thought. "Interleaved thinking" allows models to reason between tool calls — useful for sophisticated multi-step planning. Increases token usage and latency.

---

## Model Routing

### Single Model

```json
{ "model": "anthropic/claude-sonnet-4.6" }
```

### Fallback Routing

```json
{
  "models": ["anthropic/claude-sonnet-4.6", "anthropic/claude-haiku-4.5"],
  "route": "fallback"
}
```

### Provider Preferences

```json
{
  "model": "anthropic/claude-sonnet-4.6",
  "provider": {
    "order": ["Anthropic", "AWS Bedrock"],
    "allow_fallbacks": true
  }
}
```

OpenRouter automatically routes to providers that can handle your prompt size and parameters, with fallbacks for uptime.

---

## OpenRouter-Specific Features

Beyond OpenAI compatibility:

| Feature | Description |
|---------|-------------|
| Schema normalization | Single schema works across all providers/models |
| Structured outputs | `response_format` for JSON enforcement |
| Plugins | Web search, PDF parsing, response healing, context compression |
| Assistant prefill | Complete partial model responses |
| Generation Stats API | `GET /api/v1/generation?id=<X-Generation-Id>` for usage/cost data |
| Model filter | `GET /api/v1/models?supported_parameters=tools` to find tool-capable models |

---

## Claude Sonnet 4.6 on OpenRouter

**Model ID:** `anthropic/claude-sonnet-4.6`  
**Released:** February 17, 2026

### Pricing

| Token Type | Rate |
|------------|------|
| Input | $3.00 / million tokens |
| Output | $15.00 / million tokens |
| Cache write | $3.75 / million tokens (1.25x input) |
| Cache read | $0.30 / million tokens (0.1x input, 90% savings) |
| Batch API | 50% discount, async within 24h |

### Capabilities

| Feature | Supported |
|---------|-----------|
| Context window | 1,000,000 tokens |
| Max output | 128,000 tokens |
| Vision | Yes |
| Function/tool calling | Yes |
| Streaming | Yes |
| Reasoning / extended thinking | Yes |
| Prompt caching | Yes |

### Use Cases (Anthropic positioning)

- Iterative development and complex codebase navigation
- End-to-end project management with memory
- Polished document creation
- Computer use / web automation
- Coding agents

### Reasoning

Enable via the `reasoning` parameter; access results in `reasoning_details` array. Mirrors Anthropic's extended thinking API.

---

## Rust Client Options

For open-mpm, the recommended approach is `async-openai` (the most complete async Rust OpenAI client) with OpenRouter base URL:

```rust
use async_openai::{Client, config::OpenAIConfig};

let config = OpenAIConfig::new()
    .with_api_base("https://openrouter.ai/api/v1")
    .with_api_key(std::env::var("OPENROUTER_API_KEY")?);

let client = Client::with_config(config);
```

Alternatively, build a minimal HTTP client with `reqwest` + `serde_json` for full control.

---

## Rate Limits

OpenRouter does not publish fixed per-model rate limits in its public docs. Rate limits are provider-dependent and may vary. The `429` response code signals rate limit exceeded. Use exponential backoff with jitter for retries.

---

## Sources

- [OpenRouter API Reference Overview](https://openrouter.ai/docs/api/reference/overview)
- [Chat Completions API](https://openrouter.ai/docs/api/api-reference/chat/send-chat-completion-request)
- [Streaming Docs](https://openrouter.ai/docs/api/reference/streaming)
- [Tool Calling Guide](https://openrouter.ai/docs/guides/features/tool-calling)
- [Claude Sonnet 4.6 on OpenRouter](https://openrouter.ai/anthropic/claude-sonnet-4.6)
- [OpenRouter Quickstart](https://openrouter.ai/docs/quickstart)
