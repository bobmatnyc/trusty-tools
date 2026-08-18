//! trusty-code's inference layer — a CONSUMER of the shared multi-provider
//! adapter, not a provider abstraction of its own (#4425, epic #4429).
//!
//! Why: the owner directive (2026-07-30) is that all inference/chat in this
//! workspace goes through ONE adapter pattern that supports multiple providers
//! and streams when needed. Until #4425 trusty-code owned a parallel one:
//! `InferenceAdapter` (its own object-safe `chat` seam) plus duplicate
//! `ChatRequest`/`ChatResponse`/`ChatMessage`/`UsageBlock`/`InferenceError` types and
//! a `convert` module bridging them to the shared ones. That duplication had a
//! concrete cost, not just an aesthetic one: the shared adapter gained real SSE
//! streaming (`InferenceAdapter::chat_stream`, epic #3696 Gap B) and trusty-code
//! could not use it, because its own trait had no streaming method — so a `tcode
//! tui` turn could only ever arrive as one paste. #4425 deletes the parallel
//! abstraction outright: the seam every trusty-code call site depends on IS
//! `trusty_common::inference::InferenceAdapter`, and the wire types ARE the
//! shared ones (they were copied from trusty-code's in #2406, so this is a
//! de-duplication, not a re-modelling).
//! What: this module now contributes only trusty-code-SPECIFIC transports and
//! decorators — [`OpenAiCompatClient`] (OpenRouter / Fireworks / Together /
//! AtlasCloud, delegating to the shared adapters), [`BedrockChatClient`]
//! (Bedrock Converse; its re-pointing at the shared Bedrock adapter is #4426),
//! [`DispatchingLlmClient`] (per-request routing by model slug), the opt-in
//! `debug_capture` wire-dump decorator (#2264), and the `tool_call_extractor`
//! argument-repair machinery — plus flat re-exports of the shared trait, error,
//! and wire types so existing `crate::llm::…` import paths keep resolving.
//! Every one of those is an IMPLEMENTATION of the shared trait; none of them is
//! an abstraction over providers.
//! OpenRouter/Fireworks credentials resolve via the shared 3-tier chain
//! (process env > `.env.local` > secure store); Bedrock uses the standard AWS
//! credential chain (no key). Library helpers never read `std::env` for secrets
//! directly.
//! Test: `cargo test -p trusty-code` covers transport routing, the streaming
//! decorator pass-through (`dispatch::tests`, `debug_capture`), the
//! trusty-code-local response views (`response_ext`), and the offline black-box
//! HTTP round-trip in `tests/inference_shared_adapter_e2e.rs`. `cargo test -p
//! trusty-code -- --include-ignored` additionally runs the live
//! `live_openrouter_call`/`live_fireworks_call`/`live_bedrock_call` tests.
//!
//! [`OpenAiCompatClient`]: crate::llm::OpenAiCompatClient
//! [`BedrockChatClient`]: crate::llm::BedrockChatClient
//! [`DispatchingLlmClient`]: crate::llm::DispatchingLlmClient

mod bedrock;
mod client;
mod debug_capture;
mod dispatch;
mod identity;
mod response_ext;
mod tool_call_extractor;

// #4425: the two `InferenceAdapter` identity-method shapes trusty-code's
// non-provider implementors (mocks, decorators) need. Crate-internal — nothing
// outside trusty-code implements the shared trait on trusty-code's behalf.
pub(crate) use identity::{delegating_adapter_identity, mock_adapter_identity};

// ── trusty-code-specific transports, decorators, and views ────────────────────

pub use bedrock::BedrockChatClient;
pub use client::OpenAiCompatClient;
pub use debug_capture::{DebugCaptureSink, wrap_with_debug_capture};
pub use dispatch::DispatchingLlmClient;
pub use response_ext::{finish_reason, resolved_model, token_usage};
pub use tool_call_extractor::{
    DEFAULT_MAX_REPAIR_ATTEMPTS, ExtractedToolCall, ExtractionStrategy, SchemaViolation,
    ToolCallExtractError, ToolCallExtractor, extract_with_repair, strategy_order_for,
};

// ── The SHARED adapter surface, re-exported ───────────────────────────────────
//
// #4425: these are re-exports, NOT definitions. Keeping the `crate::llm::…`
// paths alive is what let the migration touch call sites mechanically instead
// of rewriting 26 files' import blocks; the types themselves are owned by
// `trusty_common::inference` and shared with every other consumer in the
// workspace.

pub use trusty_common::inference::{
    AssistantMessage, CacheControl, ChatChoice, ChatMessage, ChatRequest, ChatResponse, ChatStream,
    ChatStreamEvent, FunctionCall, FunctionDefinition, InferenceAdapter, InferenceError,
    PromptTokensDetails, RequestUsageConfig, StopReason, StreamAssembly, StreamCompletion,
    ToolCall, ToolCallDelta, ToolDefinition, UsageBlock, buffered_stream,
};
