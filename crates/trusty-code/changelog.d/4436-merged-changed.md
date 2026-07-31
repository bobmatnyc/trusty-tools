Changed

- **trusty-code no longer defines its own LLM/provider abstraction; it consumes
  the shared one (issue #4425, epic #4429).** `llm::LlmClientTrait` is deleted
  and every call site now depends on
  `trusty_common::inference::InferenceAdapter` — the same trait trusty-review
  and the shared OpenRouter/Fireworks/Together/AtlasCloud/Bedrock adapters
  already implement. With it go trusty-code's duplicate wire types
  (`ChatRequest`, `ChatResponse`, `ChatMessage`, `ToolDefinition`, `UsageBlock`,
  `LlmError`) and the `llm::convert` bridge that existed solely to translate
  between the two copies; `crate::llm` now re-exports the shared types, so
  existing `crate::llm::…` import paths are unchanged. This is what unlocked
  streaming: the shared trait already carried `chat_stream` with native SSE,
  and the deleted local trait had no streaming method at all. Net −478 lines
  across the two crates (−2,409 removed, +1,931 added, most of the additions
  being the shared `StreamAssembly` and its tests).
  `LlmError` is renamed to `InferenceError` and its `ApiError` variant to `Api`
  (rlib-consumer-visible; no in-tree consumer outside trusty-code).
- **`OpenAiCompatClient`, `DispatchingLlmClient`, `BedrockChatClient`, the
  transcript recorder, and the debug-capture decorator all implement
  `chat_stream` explicitly.** The trait's default would have buffered through
  `chat`, which would have silently disabled streaming on exactly the
  production paths (`run_task`, `task::executor`, and anything run with
  `TCODE_DEBUG_TRANSCRIPT` set) that wrap the transport in a decorator.
- **A `chat_stream` handshake failure is propagated, never silently retried as
  a blocking call.** A degraded-but-working tcode would make "is streaming
  working?" unanswerable from the outside.
- **Bedrock streaming is the buffered fallback for now.** A `bedrock/*` turn
  arrives as one content delta plus the terminal one — the pre-existing
  behaviour, with no regression. Its real `ConverseStream` transport is #4426,
  and lands entirely inside `trusty_common::inference::bedrock`.
