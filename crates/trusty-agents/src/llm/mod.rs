//! OpenRouter LLM client (via async-openai, OpenAI-compatible) + provider routing.
//!
//! Why: Centralizes client construction (base URL, API key) and exposes a
//! small, ergonomic set of chat helpers so the PM loop and sub-agent mode
//! don't duplicate async-openai request plumbing or provider routing. This
//! module is a thin facade: it wires submodules together and re-exports the
//! stable `llm::` API surface that the rest of the crate depends on.
//! What: Declares the provider adapters, the Anthropic-native / Bedrock
//! backends, the HTTP/event/compression/helper submodules, and the chat entry
//! points (single-shot in `single_turn`, multi-turn in `tool_loop`), then
//! re-exports them under their historical `llm::` paths. `inference_bridge`
//! and `inference_client` (#2410, epic #2400) are the shared-inference seam:
//! a pure `async-openai` ↔ `trusty_common::inference` type bridge plus the
//! OpenRouter transport built on it, wired into `tool_loop::turn::dispatch_turn`
//! behind the default-OFF `TAGENT_INFERENCE_SHARED` flag.
//! Test: Each submodule carries its own unit tests; the dispatch loop's
//! parallel-tool behavior is covered by `tool_loop::tests`.

pub mod adapter;
pub mod anthropic_native;
pub mod bedrock;
mod compress;
pub mod credentials;
mod events;
mod helpers;
mod http;
pub mod inference_bridge;
pub mod inference_client;
mod single_turn;
pub mod thinking_classifier;
mod tool_loop;

pub use thinking_classifier::{ThinkingMode, classify_thinking_mode};

// Public API surface preserved across the #360 split: these were all
// top-level `llm::` items before the module was decomposed.
pub use compress::{apply_compression, trim_messages_with_manager};
pub use helpers::{ChatResponse, ToolCall, create_client, should_retry_plain_text_turn};
pub use single_turn::{chat, chat_adapter_aware};
pub use tool_loop::{chat_with_tools, chat_with_tools_gated};

pub(crate) use http::http_client;
